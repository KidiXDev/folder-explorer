# Folder Explorer

A fast Windows file explorer that shows the real size of every folder. Written in Rust with a [Slint](https://slint.dev) interface.

Browse like you would in File Explorer, and every folder shows its true size, file count and folder count, so you can see at a glance what is filling your disk.

## Features

- **Folder sizes everywhere**: size, file count and folder count for every folder, filling in live as the scan runs
- **Views**: extra large, large, medium and small icons, list, details and tiles, with sizes shown in all of them
- **Thumbnails and real icons**: image and video thumbnails plus the same icons Windows shows, loaded in the background
- **File management**: new folder, inline rename, cut, copy and paste that work with File Explorer
- **Instant search**: searches the scanned tree in memory, with filters for size and for modified, created or accessed date
- **Summary pane**: see what fills a folder by file type or by age, then double-click a row to list those files
- **Checksums**: MD5, SHA-1, SHA-256 and SHA-512 columns
- **Size units**: automatic, or fixed bytes, KB, MB, GB or TB
- **Export**: save the list as comma, tab, semicolon, pipe or tilde separated text
- **File actions**: open, copy path, move to Recycle Bin, properties, show in File Explorer
- **Drive overview**: usage for every drive, plus a usage bar for the current drive
- **Honest totals**: folders that could not be fully read are marked with `≥`, and the unreadable items are listed
- **Light and dark mode** that follow Windows

## Performance

Measured with a 12-thread CPU and an NVMe SSD on a tree of 1.96 million files in 225,000 folders. Scan times are for a warm disk cache; the first scan after a restart is slower because it waits on the disk.

| | Time |
| --- | --- |
| Scan, parallel with a global lock per file (baseline) | 8.3 s |
| Scan, Folder Explorer | 2.7 s |
| Search all 1.96 million file names | 12 ms |

How it stays fast:

- Each scan thread builds its own part of the tree, so workers share no locks
- File metadata comes straight from the directory listing, with no extra call per file
- One scan feeds everything: sizes, search and summaries all read the same in-memory tree
- The list is virtual, so a million search results cost nothing until they scroll into view
- Icons and thumbnails load only for what is on screen, newest first, on their own threads
- Results from a scan are kept, so browsing into a scanned folder needs no rescan

Keeping the tree in memory has a cost: about 115 bytes per file, or 225 MB for 2 million files. Very large search results add more on top of that until you leave the results.

Sizes are logical file sizes. Files with several hard links are counted once per link. Junctions and symbolic links are listed but never followed, so nothing is counted twice.

## Download

Grab the latest `folder-explorer-*-windows-x64.zip` from the [Releases](https://github.com/KidiXDev/folder-explorer/releases) page and run `folder-explorer.exe`. Releases are built from `v*` tags.

## Build

You need [Rust](https://rustup.rs) 1.92 or newer and the MSVC build tools.

```bash
cargo run --release
```

Open a specific folder at startup:

```bash
cargo run --release -- "C:\Users"
```

Run the tests:

```bash
cargo test
```

## Keyboard shortcuts

| Keys | Action |
| --- | --- |
| Alt+Left, Alt+Right | Back, forward |
| Alt+Up, Backspace | Up one level |
| Enter | Open |
| F5 | Refresh |
| Esc | Stop scanning, or leave search results |
| Ctrl+F | Search |
| Ctrl+L | Edit the path |
| Ctrl+A | Select all |
| F2 | Rename |
| Ctrl+Shift+N | New folder |
| Ctrl+X, Ctrl+C, Ctrl+V | Cut, copy, paste |
| Delete | Move to Recycle Bin |

## Project layout

| Path | Purpose |
| --- | --- |
| `src/scan.rs` | Parallel scanner and the cache of scanned trees |
| `src/query.rs` | In-memory search and summaries |
| `src/list.rs` | Virtual list model: sorting and selection |
| `src/item.rs` | List rows and column definitions |
| `src/app.rs` | Application state and background jobs |
| `src/icons.rs` | Shell icons and thumbnails, loaded in the background |
| `src/file_ops.rs` | Name checks and new folders |
| `src/platform.rs` | Windows integration |
| `ui/` | Slint interface and icons |

## Settings

Settings are stored in `%APPDATA%\FolderExplorer\settings.ini`.

## License

[MIT](LICENSE)
