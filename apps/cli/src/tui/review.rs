//! Looking at a link before it downloads: the engine says whether it is a
//! file, a video or audio page, or a web page; a card shows what will be
//! saved (quality, type, size, folder) and nothing starts until the person
//! confirms.

use super::flows;
use super::prompt::{Action, Prompt};
use crate::client::{self, Engine};
use crate::download;
use crate::queue;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{
    LinkInspection, LinkKind, MediaInspection, MediaVariant, MediaVariantKind,
};
use fetchpath_protocol::{JobSnapshot, ProtocolError, SensitiveUrl, Timestamp};
use std::path::PathBuf;

/// A link waiting to be looked at and confirmed, with the options typed
/// after it.
#[derive(Clone, Debug)]
pub struct Draft {
    pub link: String,
    pub url: SensitiveUrl,
    pub to: Option<String>,
    pub at: Option<Timestamp>,
    pub sha256: Option<String>,
    pub quality: Option<String>,
}

/// What the engine found.
#[derive(Clone, Debug)]
pub struct Look {
    pub link: LinkInspection,
    pub media: Option<MediaInspection>,
    /// Why a media page could not be inspected (tools missing, the site
    /// refused), shown instead of a quality list.
    pub media_error: Option<String>,
    /// The video tools are not set up (`media.helper_unavailable`), so the
    /// card offers to set them up.
    pub tools_missing: bool,
    /// Where a download goes without `--to`: the matching rule's folder,
    /// else the default one.
    pub folder: Option<PathBuf>,
}

impl Look {
    /// The rule that decides for this link, if one matches.
    pub fn rule(&self) -> Option<&fetchpath_protocol::model::Rule> {
        self.link.rules.as_ref()?.matched.as_ref()
    }
}

/// Asks the engine about a link: its kind, then for a media page (or a web
/// page that may hold a video) the formats it offers. This can take seconds
/// for a media page, so the inline view calls it off its own thread.
pub fn look(engine: &Engine, draft: &Draft) -> Result<Look, ProtocolError> {
    let link = match engine.send(Command::InspectLink {
        url: draft.url.clone(),
    })? {
        CommandResult::LinkInspection { inspection } => inspection,
        other => return Err(client::unexpected(&other)),
    };
    let mut tools_missing = false;
    let (media, media_error) = match link.kind {
        LinkKind::MediaPage | LinkKind::WebPage => match engine.send(Command::InspectMedia {
            url: draft.url.clone(),
        }) {
            Ok(CommandResult::MediaInspection { inspection })
                if !inspection.variants.is_empty() =>
            {
                (Some(inspection), None)
            }
            Ok(_) => (None, Some("The page offers no video or audio.".to_owned())),
            Err(error) => {
                tools_missing = error.code.as_str() == TOOLS_MISSING;
                (None, Some(error.message))
            }
        },
        _ => (None, None),
    };
    let ruled = link
        .rules
        .as_ref()
        .and_then(|verdict| verdict.matched.as_ref()?.spec.then.folder.clone());
    let folder = match ruled {
        Some(folder) => Some(PathBuf::from(folder)),
        None => queue::default_folder(engine)?,
    };
    Ok(Look {
        link,
        media,
        media_error,
        tools_missing,
        folder,
    })
}

/// The error code for video tools that are not set up.
pub const TOOLS_MISSING: &str = "media.helper_unavailable";

pub enum Stage {
    Looking,
    Ready(Box<Look>),
    Failed(String),
}

/// What a key did to the card.
#[derive(Debug, Eq, PartialEq)]
pub enum Answer {
    None,
    Confirm,
    Cancel,
    /// Set up the video tools, then look at this link again.
    SetUpTools,
}

/// A value being typed on the card.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Field {
    Checksum,
    Name,
}

pub struct Card {
    pub draft: Draft,
    pub stage: Stage,
    /// Formats in the order shown: videos tallest first, then audio.
    order: Vec<usize>,
    /// The chosen row in `order`.
    pub choice: usize,
    /// A file name the person typed instead of the suggested one.
    pub name: Option<String>,
    /// Paths other cards in the same batch will save to, so two links
    /// with the same name do not clash.
    pub taken: Vec<PathBuf>,
    /// A value being typed, and what was wrong with the last one.
    pub editing: Option<(Field, Prompt)>,
    pub problem: Option<String>,
}

impl Card {
    pub fn new(draft: Draft) -> Self {
        Self {
            draft,
            stage: Stage::Looking,
            order: Vec::new(),
            choice: 0,
            name: None,
            taken: Vec::new(),
            editing: None,
            problem: None,
        }
    }

    pub fn set(&mut self, result: Result<Look, ProtocolError>) {
        match result {
            Ok(look) => {
                if let Some(media) = &look.media {
                    self.order = variant_order(&media.variants);
                    // The quality typed, else the matching rule's.
                    let ruled = look
                        .rule()
                        .and_then(|rule| rule.spec.then.media_quality.as_deref());
                    self.choice = self
                        .draft
                        .quality
                        .as_deref()
                        .or(ruled)
                        .and_then(|quality| queue::pick_variant(media, quality).ok())
                        .and_then(|picked| {
                            self.order
                                .iter()
                                .position(|&index| std::ptr::eq(&media.variants[index], picked))
                        })
                        .unwrap_or_else(|| recommended(&media.variants, &self.order));
                }
                self.stage = Stage::Ready(Box::new(look));
            }
            Err(error) => self.stage = Stage::Failed(error.message),
        }
    }

    /// The row chosen when nothing was asked for: the best video up to
    /// 1080p, as the desktop picks.
    pub fn recommended(&self) -> usize {
        match self.look().and_then(|look| look.media.as_ref()) {
            Some(media) => recommended(&media.variants, &self.order),
            None => 0,
        }
    }

    pub fn look(&self) -> Option<&Look> {
        match &self.stage {
            Stage::Ready(look) => Some(look),
            _ => None,
        }
    }

    /// The formats in display order.
    pub fn variants(&self) -> Vec<&MediaVariant> {
        match self.look().and_then(|look| look.media.as_ref()) {
            Some(media) => self
                .order
                .iter()
                .map(|&index| &media.variants[index])
                .collect(),
            None => Vec::new(),
        }
    }

    /// The video tools are needed for this link and are not set up.
    pub fn tools_missing(&self) -> bool {
        self.look().is_some_and(|look| look.tools_missing)
    }

    pub fn is_media(&self) -> bool {
        !self.variants().is_empty()
    }

    /// Whether Enter would start something: a media page must have formats
    /// to choose from; a file or a page can be saved as it is unless a rule
    /// requires a checksum that was not given.
    pub fn can_confirm(&self) -> bool {
        match &self.stage {
            Stage::Looking => false,
            Stage::Failed(_) => true,
            Stage::Ready(look) => {
                (look.link.kind != LinkKind::MediaPage || self.is_media()) && !self.needs_checksum()
            }
        }
    }

    /// A matching rule requires a checksum for this file and none was given.
    pub fn needs_checksum(&self) -> bool {
        !self.is_media()
            && self.draft.sha256.is_none()
            && self
                .look()
                .and_then(Look::rule)
                .is_some_and(|rule| rule.spec.then.require_checksum)
    }

    pub fn choose(&mut self, row: usize) {
        let count = self.variants().len();
        if count > 0 {
            self.choice = row.min(count - 1);
        }
    }

    /// Whether a checksum can be given: a file or a page, not a video.
    pub fn takes_checksum(&self) -> bool {
        !matches!(self.stage, Stage::Looking) && !self.is_media()
    }

    /// Starts typing a value on the card, prefilled with the current one.
    pub fn edit(&mut self, field: Field) {
        let mut prompt = Prompt::default();
        prompt.set(match field {
            Field::Checksum => self.draft.sha256.clone().unwrap_or_default(),
            Field::Name => self.file_name(),
        });
        self.problem = None;
        self.editing = Some((field, prompt));
    }

    /// Takes a typed checksum or name; `false` with a problem set when it
    /// cannot be used.
    pub fn apply(&mut self, field: Field, text: &str) -> bool {
        let text = text.trim();
        let outcome = match field {
            Field::Checksum if text.is_empty() => {
                self.draft.sha256 = None;
                Ok(())
            }
            Field::Checksum => flows::checksum(text).map(|hex| self.draft.sha256 = Some(hex)),
            Field::Name => flows::file_name(text).map(|name| self.name = Some(name)),
        };
        match outcome {
            Ok(()) => {
                self.problem = None;
                true
            }
            Err(problem) => {
                self.problem = Some(problem);
                false
            }
        }
    }

    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> Answer {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        if key.kind == KeyEventKind::Release {
            return Answer::None;
        }
        if let Some((field, prompt)) = &mut self.editing {
            let field = *field;
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    self.problem = None;
                }
                _ => {
                    if let Action::Submit(text) = prompt.handle(key)
                        && self.apply(field, &text)
                    {
                        self.editing = None;
                    }
                }
            }
            return Answer::None;
        }
        match key.code {
            KeyCode::Char('s' | 'S') if self.takes_checksum() => {
                self.edit(Field::Checksum);
                Answer::None
            }
            KeyCode::Char('n' | 'N') if !matches!(self.stage, Stage::Looking) => {
                self.edit(Field::Name);
                Answer::None
            }
            KeyCode::Esc => Answer::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Answer::Cancel,
            KeyCode::Enter if self.can_confirm() => Answer::Confirm,
            KeyCode::Char('i' | 'I') if self.tools_missing() => Answer::SetUpTools,
            KeyCode::Up | KeyCode::Char('k') => {
                self.choice = self.choice.saturating_sub(1);
                Answer::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.choose(self.choice + 1);
                Answer::None
            }
            KeyCode::Home => {
                self.choose(0);
                Answer::None
            }
            KeyCode::End => {
                self.choose(usize::MAX);
                Answer::None
            }
            KeyCode::Char(digit @ '1'..='9') => {
                self.choose(digit as usize - '1' as usize);
                Answer::None
            }
            _ => Answer::None,
        }
    }

    /// The file name asked for: typed by the person, else suggested.
    pub fn file_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.suggested_name())
    }

    /// The name the link suggests.
    fn suggested_name(&self) -> String {
        let fallback = download::file_name_from_url(&self.draft.link);
        let Some(look) = self.look() else {
            return fallback;
        };
        if let (Some(media), Some(variant)) = (&look.media, self.variants().get(self.choice)) {
            return format!(
                "{}.{}",
                queue::safe_title(&media.title, &self.draft.link),
                variant.extension
            );
        }
        let named = look
            .link
            .file_name
            .as_deref()
            .and_then(download::safe_file_name)
            .unwrap_or(fallback);
        if look.link.kind == LinkKind::WebPage && !named.contains('.') {
            format!("{named}.html")
        } else {
            named
        }
    }

    /// The path asked for, before any clash is avoided.
    fn wanted(&self, folder: Option<&std::path::Path>) -> Result<PathBuf, String> {
        let path = queue::destination_path(self.draft.to.as_deref(), folder, &self.file_name())?;
        Ok(match &self.name {
            // `--to` may name a file; a typed name replaces its name.
            Some(name) => path.with_file_name(name),
            None => path,
        })
    }

    /// The name of a file already where this download would go (or taken
    /// by another link in the batch), which it will not replace.
    pub fn clash(&self) -> Option<String> {
        let folder = self.look().and_then(|look| look.folder.clone());
        let wanted = self.wanted(folder.as_deref()).ok()?;
        flows::occupied(&wanted, &self.taken).then(|| {
            wanted
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }

    /// The full path the download will be saved as: the one asked for, or
    /// when a file is already there, the first free "name (N)" beside it.
    pub fn destination(&self) -> Result<PathBuf, String> {
        let folder = self.look().and_then(|look| look.folder.clone());
        self.wanted(folder.as_deref())
            .map(|wanted| flows::beside(&wanted, &self.taken))
    }

    /// Queues the download as shown.
    pub fn confirm(&self, engine: &Engine) -> Result<JobSnapshot, ProtocolError> {
        let folder = match self.look() {
            Some(look) => look.folder.clone(),
            // Looking failed, so the default folder was never fetched.
            None => queue::default_folder(engine)?,
        };
        let destination = self
            .wanted(folder.as_deref())
            .map(|wanted| flows::beside(&wanted, &self.taken))
            .map_err(|message| client::input_error(&message))?;
        let media = self
            .variants()
            .get(self.choice)
            .map(|variant| (variant.id.clone(), variant.label.clone()));
        queue::create_job(
            engine,
            self.draft.url.clone(),
            &destination,
            self.draft.at,
            self.draft.sha256.clone(),
            media,
        )
    }
}

fn recommended(variants: &[MediaVariant], order: &[usize]) -> usize {
    order
        .iter()
        .position(|&index| {
            let variant = &variants[index];
            variant.kind == MediaVariantKind::Video && variant.height.unwrap_or(0) <= 1080
        })
        .unwrap_or(0)
}

fn variant_order(variants: &[MediaVariant]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..variants.len()).collect();
    order.sort_by_key(|&index| {
        let variant = &variants[index];
        let kind = match variant.kind {
            MediaVariantKind::Video => 0,
            MediaVariantKind::Audio => 1,
            MediaVariantKind::Unknown => 2,
        };
        (
            kind,
            std::cmp::Reverse(variant.height.unwrap_or(0)),
            std::cmp::Reverse(variant.fps.unwrap_or(0)),
            index,
        )
    });
    order
}

/// A plain word for what a file is, from its name first and its media type
/// second.
pub fn kind_label(content_type: Option<&str>, name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let by_name = match extension.as_str() {
        "iso" | "img" | "dmg" | "vhd" | "vhdx" => Some("Disk image"),
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "cab" => {
            Some("Archive")
        }
        "exe" | "msi" | "msix" | "appx" | "msixbundle" | "apk" | "deb" | "rpm" | "pkg" => {
            Some("Program")
        }
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" => Some("Video"),
        "mp3" | "flac" | "m4a" | "wav" | "ogg" | "opus" | "aac" => Some("Audio"),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" => Some("Image"),
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "epub" | "md" => Some("Document"),
        "xls" | "xlsx" | "ods" | "csv" => Some("Spreadsheet"),
        "ppt" | "pptx" | "odp" => Some("Presentation"),
        "html" | "htm" => Some("Web page"),
        _ => None,
    };
    if let Some(label) = by_name {
        return label;
    }
    match content_type.unwrap_or_default() {
        "text/html" | "application/xhtml+xml" => "Web page",
        "application/pdf" => "Document",
        "application/zip" | "application/x-7z-compressed" | "application/gzip" => "Archive",
        other if other.starts_with("video/") => "Video",
        other if other.starts_with("audio/") => "Audio",
        other if other.starts_with("image/") => "Image",
        other if other.starts_with("text/") => "Text",
        _ => "File",
    }
}

/// The site a link is on, without `www.`.
pub fn host(link: &str) -> String {
    url::Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| host.trim_start_matches("www.").to_owned())
        .unwrap_or_default()
}

/// One format as a row: its label (the helper's, which already names the
/// frame rate), its container, and "audio only" where that applies.
pub fn variant_text(variant: &MediaVariant) -> String {
    let kind = match variant.kind {
        MediaVariantKind::Video => "",
        MediaVariantKind::Audio => "  audio only",
        MediaVariantKind::Unknown => "  other",
    };
    format!("{}  {}{kind}", variant.label, variant.extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(link: &str, quality: Option<&str>) -> Draft {
        Draft {
            link: link.to_owned(),
            url: SensitiveUrl::try_from(link.to_owned()).unwrap(),
            to: Some(r"C:\Users\person\Downloads\".to_owned()),
            at: None,
            sha256: None,
            quality: quality.map(str::to_owned),
        }
    }

    fn variant(id: &str, label: &str, kind: MediaVariantKind, height: Option<u32>) -> MediaVariant {
        MediaVariant {
            id: id.into(),
            label: label.into(),
            kind,
            extension: if kind == MediaVariantKind::Audio {
                "m4a"
            } else {
                "mp4"
            }
            .into(),
            height,
            fps: None,
        }
    }

    fn media_look() -> Look {
        Look {
            link: LinkInspection {
                kind: LinkKind::MediaPage,
                file_name: None,
                content_type: None,
                size_bytes: None,
                resumable: false,
                rules: None,
            },
            media: Some(MediaInspection {
                title: "Big Buck: Bunny?".into(),
                duration_seconds: Some(596.0),
                variants: vec![
                    variant("140", "Audio", MediaVariantKind::Audio, None),
                    variant("22", "720p", MediaVariantKind::Video, Some(720)),
                    variant("137", "1080p", MediaVariantKind::Video, Some(1080)),
                ],
            }),
            media_error: None,
            tools_missing: false,
            folder: None,
        }
    }

    #[test]
    fn a_media_card_offers_the_tallest_video_first_and_moves_with_keys() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut card = Card::new(draft("https://www.youtube.com/watch?v=x", None));
        assert!(!card.can_confirm());
        card.set(Ok(media_look()));
        let labels: Vec<&str> = card.variants().iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["1080p", "720p", "Audio"]);
        assert_eq!(card.file_name(), "Big Buck_ Bunny_.mp4");
        let press = |card: &mut Card, code| card.key(KeyEvent::new(code, KeyModifiers::NONE));
        press(&mut card, KeyCode::Down);
        press(&mut card, KeyCode::Down);
        press(&mut card, KeyCode::Down);
        assert_eq!(card.choice, 2);
        assert_eq!(card.file_name(), "Big Buck_ Bunny_.m4a");
        press(&mut card, KeyCode::Char('2'));
        assert_eq!(card.variants()[card.choice].label, "720p");
        assert_eq!(press(&mut card, KeyCode::Enter), Answer::Confirm);
        assert_eq!(press(&mut card, KeyCode::Esc), Answer::Cancel);
        assert_eq!(
            card.destination().unwrap(),
            PathBuf::from(r"C:\Users\person\Downloads\Big Buck_ Bunny_.mp4")
        );
    }

    #[test]
    fn a_quality_typed_with_the_link_is_preselected() {
        let mut card = Card::new(draft("https://youtu.be/x", Some("audio")));
        card.set(Ok(media_look()));
        assert_eq!(card.variants()[card.choice].label, "Audio");
    }

    #[test]
    fn without_a_quality_the_best_video_up_to_1080p_is_chosen() {
        let mut look = media_look();
        look.media.as_mut().unwrap().variants.push(variant(
            "313",
            "2160p",
            MediaVariantKind::Video,
            Some(2160),
        ));
        let mut card = Card::new(draft("https://youtu.be/x", None));
        card.set(Ok(look));
        assert_eq!(card.variants()[0].label, "2160p");
        assert_eq!(card.variants()[card.choice].label, "1080p");
        assert_eq!(card.recommended(), card.choice);
    }

    #[test]
    fn a_media_page_without_formats_cannot_start_but_a_web_page_can() {
        let mut look = media_look();
        look.media = None;
        look.media_error = Some("media.helper_unavailable: not set up".into());
        look.tools_missing = true;
        let mut card = Card::new(draft("https://youtu.be/x", None));
        card.set(Ok(look.clone()));
        assert!(!card.can_confirm());
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |card: &mut Card, code| card.key(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(press(&mut card, KeyCode::Enter), Answer::None);
        assert_eq!(press(&mut card, KeyCode::Char('i')), Answer::SetUpTools);

        look.link.kind = LinkKind::WebPage;
        let mut page = Card::new(draft("https://example.com/about", None));
        page.set(Ok(look));
        assert!(page.can_confirm());
        assert_eq!(page.file_name(), "about.html");
    }

    #[test]
    fn a_file_is_named_by_its_server_safely_and_labelled_by_type() {
        let mut card = Card::new(draft("https://a.test/get?id=9", None));
        card.set(Ok(Look {
            link: LinkInspection {
                kind: LinkKind::File,
                file_name: Some("..\\evil:name.iso".into()),
                content_type: Some("application/octet-stream".into()),
                size_bytes: Some(6_000_000_000),
                resumable: true,
                rules: None,
            },
            media: None,
            media_error: None,
            tools_missing: false,
            folder: None,
        }));
        assert!(card.can_confirm());
        let name = card.file_name();
        assert!(!name.contains(['\\', ':']), "{name}");
        assert_eq!(
            kind_label(Some("application/octet-stream"), &name),
            "Disk image"
        );
        assert_eq!(kind_label(Some("video/mp4"), "watch"), "Video");
        assert_eq!(kind_label(None, "x"), "File");
        assert_eq!(host("https://www.youtube.com/watch?v=1"), "youtube.com");
    }

    fn file_look(folder: &std::path::Path, require_checksum: bool) -> Look {
        let rules = require_checksum.then(|| {
            serde_json::from_value(serde_json::json!({
                "matched": {
                    "id": 1,
                    "when": { "file_types": ["iso"] },
                    "then": { "require_checksum": true },
                },
                "checks": [],
            }))
            .unwrap()
        });
        Look {
            link: LinkInspection {
                kind: LinkKind::File,
                file_name: Some("disc.iso".into()),
                content_type: None,
                size_bytes: Some(2048),
                resumable: true,
                rules,
            },
            media: None,
            media_error: None,
            tools_missing: false,
            folder: Some(folder.to_path_buf()),
        }
    }

    fn typed(card: &mut Card, text: &str) {
        use crossterm::event::{KeyCode, KeyEvent};
        for c in text.chars() {
            card.key(KeyEvent::from(KeyCode::Char(c)));
        }
        card.key(KeyEvent::from(KeyCode::Enter));
    }

    #[test]
    fn a_checksum_is_typed_on_the_card_normalized_and_unlocks_a_rule() {
        use crossterm::event::{KeyCode, KeyEvent};
        let dir = tempfile::tempdir().unwrap();
        let mut card = Card::new(Draft {
            to: None,
            ..draft("https://a.test/disc.iso", None)
        });
        card.set(Ok(file_look(dir.path(), true)));
        assert!(card.needs_checksum() && !card.can_confirm());
        assert_eq!(card.key(KeyEvent::from(KeyCode::Char('s'))), Answer::None);
        typed(&mut card, "12ab");
        assert!(
            card.editing.is_some(),
            "a wrong checksum keeps the line open"
        );
        assert_eq!(
            card.problem.as_deref(),
            Some("That is not a SHA-256: it needs 64 hexadecimal digits (4 found).")
        );
        card.key(KeyEvent::from(KeyCode::Esc));
        assert!(card.editing.is_none() && card.draft.sha256.is_none());
        card.key(KeyEvent::from(KeyCode::Char('S')));
        // The prompt starts with what is there; clear it first.
        card.editing.as_mut().unwrap().1.set(String::new());
        typed(&mut card, &format!("SHA256:{}", "AB".repeat(32)));
        assert_eq!(card.draft.sha256, Some("ab".repeat(32)));
        assert!(card.editing.is_none() && card.can_confirm());
    }

    #[test]
    fn an_existing_file_is_never_replaced_and_a_typed_name_is_used() {
        use crossterm::event::{KeyCode, KeyEvent};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("disc.iso"), b"x").unwrap();
        let mut card = Card::new(Draft {
            to: None,
            ..draft("https://a.test/disc.iso", None)
        });
        card.set(Ok(file_look(dir.path(), false)));
        assert_eq!(card.clash().as_deref(), Some("disc.iso"));
        assert_eq!(card.destination().unwrap(), dir.path().join("disc (1).iso"));
        card.key(KeyEvent::from(KeyCode::Char('n')));
        card.editing.as_mut().unwrap().1.set(String::new());
        typed(&mut card, r"a\b.iso");
        assert_eq!(
            card.problem.as_deref(),
            Some("Type a file name only; the folder stays as shown.")
        );
        card.editing.as_mut().unwrap().1.set(String::new());
        typed(&mut card, "backup.iso");
        assert_eq!(card.clash(), None);
        assert_eq!(card.destination().unwrap(), dir.path().join("backup.iso"));
    }
}
