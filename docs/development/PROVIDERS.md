# Model and dataset providers

FP-022, 28 September 2026. Hugging Face is the one provider.

## What exists

`adapters/providers` parses `hf://[datasets/|spaces/]OWNER/NAME[@REVISION][/PATH]`
and `huggingface.co` repository links (`tree`, `blob`, `resolve`, percent-encoded
revisions such as `refs/pr/3`), and resolves one through
`/api/{models|datasets|spaces}/REPO/revision/REV?blobs=true`, read with
`fetchpath_http::fetch_small` (HTTP(S) only, 32 MiB cap, 30 s). The listing gives:

- **The commit.** Every file's link is `/resolve/COMMIT/PATH`, so a branch
  that moves during the download cannot mix two versions.
- **Each file's identity.** Large (LFS or Xet) files carry the SHA-256
  Hugging Face states; the job publishes nothing that does not match, and the
  file then enters [the cache](CACHE-AND-LAN.md). Small files carry only a
  size and are pinned by the commit.
- **Safe paths.** A path that is absolute, climbs with `..`, names a Windows
  device, or holds `\ : * ? " < > |` or control characters is left out and
  reported. At most 10,000 files.

A gated or private repository (401/403) is refused in plain words: Fetchpath
holds no Hugging Face sign-in. 404 reads as no such repository or revision.

The protocol has one new query, `InspectRepository`, answering
`RepositoryView`; agents may use it, like `InspectLink`. Clients turn it into
ordinary file jobs with `RepositoryView::requests(folder)`, one `CreateJob`
per file (a batch keeps its meaning: pasted links, one folder, no
checksums), into `FOLDER\NAME\` with the repository's layout. The session now
creates a job's missing folder as the job starts, after any approval, never
while it waits for one. Clients: `fetchpath add`, the terminal's `/add` and
the desktop's Add download, which shows the commit, file count, size and how
many files are checked before anything is queued.

## How it was verified

- Unit tests: link forms and refusals, a listing pinned to its commit with
  stated hashes, unsafe paths and bad hashes left out, 401/403/404 and
  malformed or oversized answers. Protocol round trip and
  `a_repository_becomes_one_checked_download_per_file_inside_its_own_folder`;
  session `a_download_into_a_folder_not_yet_made_gets_it_as_it_starts`.
- Live, against the official client (`tools/bench/provider-compare.py`,
  `huggingface_hub` 2.0.0 with `hf_xet` 1.6.0, three runs each from empty
  caches): the same commit gave byte-identical files every time.
  [evidence](evidence/providers/fp022-compare.json)

| Case | Official client | Fetchpath before | Fetchpath after |
| --- | --- | --- | --- |
| `hf-internal-testing/tiny-random-gpt2`, 10 files | 2.5 s | 10.1 s | 4.9 s |
| `google-bert/bert-base-uncased` `model.safetensors`, 440 MB on Xet | 10.3 s | 269 s | 24.4 s |

A scripted run of the release desktop typed `hf://hf-internal-testing/tiny-random-gpt2`
into Add download: the note named the commit (`71034c5d8bde`), 10 files,
11.9 MB and 3 checked files, the folder field held the default folder, and
Start queued the files into `tiny-random-gpt2\` inside it. That run caught
the folder suggestion nesting the repository one level too deep, now fixed.

The first comparison exposed two defects in the general HTTP path, fixed and
recorded in [adaptive HTTP](ADAPTIVE-HTTP.md). Fetchpath is still slower than
the official client, which reconstructs Xet files from chunks; no speed claim
is made.

## Limitations

1. **Public repositories only**; no Hugging Face token.
2. **Xet is used through Hugging Face's HTTP bridge**, not by chunk
   reconstruction, so there is no chunk-level deduplication across versions,
   and every range follows the redirect to the CDN.
3. **Small files are not checked by a hash**, only pinned by the commit (the
   API gives their git blob SHA-1, which the job cannot check yet).
4. **No filter beyond one path**; no include or exclude patterns.
5. **Not all-or-nothing**: a repository is queued file by file, so a failure
   part way leaves the files already queued.
