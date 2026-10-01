# Changelog

All notable changes to this project are documented in this file.

## [0.1.0] - 2026-10-01

### Added
- Processes page with app grouping, heat-map columns, search and column chooser.
- Performance page: CPU (overall/per core), memory, disks, network adapters and GPUs (AMD, Intel, NVIDIA).
- App history, Startup apps, Users, Details and Services pages.
- Process actions: end task, end process tree, kill, suspend/resume, efficiency mode, priority, affinity,
  open file location, search online, copy, properties, run new task, retry as administrator.
- Linux backend reading `/proc` and `/sys` directly; Windows backend using sysinfo and Win32.
- Light/dark/system theme, zoom, update speed, persistent settings, keyboard shortcuts.
- Packaging: .deb, .rpm, tarball with installer, Arch PKGBUILD, Windows zip; CI for Linux distributions and Windows.
