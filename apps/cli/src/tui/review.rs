//! Looking at a link before it downloads: the engine says whether it is a
//! file, a video or audio page, or a web page; a card shows what will be
//! saved (quality, type, size, folder) and nothing starts until the person
//! confirms.

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
    /// Where a download goes without `--to`.
    pub folder: Option<PathBuf>,
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
    Ok(Look {
        link,
        media,
        media_error,
        tools_missing,
        folder: queue::default_folder(engine)?,
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

pub struct Card {
    pub draft: Draft,
    pub stage: Stage,
    /// Formats in the order shown: videos tallest first, then audio.
    order: Vec<usize>,
    /// The chosen row in `order`.
    pub choice: usize,
}

impl Card {
    pub fn new(draft: Draft) -> Self {
        Self {
            draft,
            stage: Stage::Looking,
            order: Vec::new(),
            choice: 0,
        }
    }

    pub fn set(&mut self, result: Result<Look, ProtocolError>) {
        match result {
            Ok(look) => {
                if let Some(media) = &look.media {
                    self.order = variant_order(&media.variants);
                    self.choice = self
                        .draft
                        .quality
                        .as_deref()
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
    /// to choose from; a file or a page can always be saved as it is.
    pub fn can_confirm(&self) -> bool {
        match &self.stage {
            Stage::Looking => false,
            Stage::Failed(_) => true,
            Stage::Ready(look) => look.link.kind != LinkKind::MediaPage || self.is_media(),
        }
    }

    pub fn choose(&mut self, row: usize) {
        let count = self.variants().len();
        if count > 0 {
            self.choice = row.min(count - 1);
        }
    }

    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> Answer {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        if key.kind == KeyEventKind::Release {
            return Answer::None;
        }
        match key.code {
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

    /// The file name that will be used, before the engine resolves any
    /// clash with an existing file.
    pub fn file_name(&self) -> String {
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

    /// The full path the download will be saved as.
    pub fn destination(&self) -> Result<PathBuf, String> {
        let folder = self.look().and_then(|look| look.folder.clone());
        queue::destination_path(
            self.draft.to.as_deref(),
            folder.as_deref(),
            &self.file_name(),
        )
    }

    /// Queues the download as shown.
    pub fn confirm(&self, engine: &Engine) -> Result<JobSnapshot, ProtocolError> {
        let folder = match self.look() {
            Some(look) => look.folder.clone(),
            // Looking failed, so the default folder was never fetched.
            None => queue::default_folder(engine)?,
        };
        let destination = queue::destination_path(
            self.draft.to.as_deref(),
            folder.as_deref(),
            &self.file_name(),
        )
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
}
