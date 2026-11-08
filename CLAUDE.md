# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Folder Explorer is a Windows-only file explorer (Rust + [Slint](https://slint.dev)) that shows the real size of every folder. Windows APIs are used directly (`windows`, `windows-sys`), so it only builds on Windows with the MSVC toolchain. Requires Rust 1.92+ (edition 2024).

## Commands

```bash
cargo run --release                  # run the app
cargo run --release -- "C:\Users"    # start scanning a folder with no input needed
cargo test                           # all tests
cargo test hash_names_cover          # one test, by name substring
```

There is no CI, lint config or rustfmt config. Tests are inline `#[cfg(test)]` modules in `src/*.rs`. `build.rs` compiles `ui/app.slint` with the `fluent` style; Slint errors surface as build errors. A `slint` agent skill is vendored in `.agents/skills/slint` (symlinked from `.claude/skills`); use it when editing `.slint` files.

## Architecture

**Single process, one UI thread, background workers.** `main.rs` installs mimalloc, builds the global rayon pool with 16 MB stacks (deep directory recursion), and calls `app::run`.

- `app.rs` holds `App` (all state), wrapped as `Shared = Rc<RefCell<App>>`. Workers never touch it: they send `Message` variants over an `mpsc` channel, and a 16 ms `pump` timer on the UI thread drains them. Messages carry a `scan` / `search` / `run` id and stale ones are dropped, which is how cancellation and navigation races are handled. `wire()` connects Slint callbacks; `run_action` dispatches string-named menu/command actions.
- `scan.rs` is the parallel scanner. Each rayon task builds its own `DirNode` subtree (no shared locks) and `Cache` keeps finished trees so browsing into a scanned folder needs no rescan. One scanned tree feeds sizes, search and summaries.
- `query.rs` searches and summarizes the in-memory tree (no disk access). `list.rs` is the virtual `ListModel` (sorting, selection) so huge result sets cost nothing until scrolled into view. `item.rs` defines rows and `Column`.
- `icons.rs` loads shell icons/thumbnails on their own threads, only for visible rows. `sidebar.rs` is the tree plus pinned quick-access folders. `platform.rs` is all Win32 integration (drives, clipboard, shell operations, window frame, dark menus). `settings.rs` persists to `%APPDATA%\FolderExplorer\settings.ini`. `error_log.rs` appends panics (with backtrace), unhandled native exceptions and failed file operations to `errors.log` in the same folder, rotating to `errors.old.log` past 512 KB.
- `ui/` is the Slint layer: `app.slint` (`AppWindow`, custom `TitleBar`), `panes.slint` (sidebar, drives view, summary, errors), `file_table.slint` / `file_grid.slint` (details vs icon views), `controls.slint`, `theme.slint`, `types.slint` (structs shared with Rust). Rust sees them through `ui::*` (`slint::include_modules!()` in `main.rs`).

## Non-obvious constraints

- **`scan.rs::is_link`** falls back to `symlink_metadata`: NTFS can report a real junction as a plain directory via `DirEntry::file_type()` (lazy reparse tag). Removing the fallback double-counted ~5,500 files in pnpm trees. Don't skip all reparse dirs either; OneDrive placeholders are real folders. Junctions/symlinks are listed but never followed.
- **Scanner I/O**: `std::fs::read_dir` + rayon + mimalloc measured faster than `FindFirstFileEx`, `GetFileInformationByHandleEx` and `NtQueryDirectoryFile`. More threads than logical CPUs didn't help. Measure before rewriting with unsafe APIs.
- **Never run UI-path work on the global rayon pool**: a running scan saturates it and the work stalls. `platform::drives` uses `thread::scope` for this reason.
- **Rename uses `MoveFileW`**, not `std::fs::rename` (which silently overwrites on Windows). Delete/copy/move go through `IFileOperation` (`platform::file_operation`, one STA per worker thread) so the shell handles conflicts, undo and the progress dialog.
- **File operations pass `FOFX_NOCOPYHOOKS`** so they can run in parallel. shell32 shares third-party copy hooks across threads, and concurrent deletes crashed inside FileZilla's (`fzshellext_64`: heap corruption via `SHFileOperationW`, access violation via `IFileOperation` without the flag). `SHFileOperationW` can't skip hooks. Regression check: `cargo test concurrent_deletes -- --ignored`.
- **Native menus and message boxes run nested message loops.** Never call them while holding the `App` `RefCell` borrow; timers use `try_borrow_mut`. Context menus are native Win32 (muda), so Slint's palette doesn't style them; dark menus come from uxtheme ordinals 135/136 in `platform::allow_dark_menus`.
- **Custom window frame**: `platform::restore_frame_appearance` must go through `upgrade_in_event_loop` because winit creates the HWND only in `resumed` (Slint timers fire before it). Both DWM calls are needed: a `no-frame` window has no shadow without the frame extension and square corners without the corner preference. `WindowMoveArea` sits behind the layout so controls on top win the press.
- **`FileTable` and `FileGrid` both stay instantiated**; the hidden one gets empty `rows`, otherwise both request icons for every visible row.
- **Grid names** use `max-lines: 2` (a wrapped `Text` reports its full wrapped height as layout minimum) and the name box is `preferred-height + 8px`, vertically centered, because Slint drops a line whose top is above the box and CJK fallback fonts overshoot the line height.
