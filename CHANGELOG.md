# Changelog

All notable changes to this project are documented in this file.

## [0.2.0] - 2026-10-04

### Added
- Temperatures everywhere: CPU (package and per core), memory modules, every disk (NVMe, SATA via `drivetemp`),
  network adapters, GPUs and batteries — shown in the sidebar, the overview tiles, the status bar and as graphs.
- **Sensors** view on the Performance page listing every hardware sensor (temperatures, fans, voltages, currents, power)
  grouped by chip, with warning and critical thresholds.
- **Battery** view: charge and power-draw graphs, energy, health (full vs. design capacity), cycle count, time remaining.
- GPU power draw, fan speed and clock; Wi-Fi signal strength; context switches, interrupts and per-core frequencies.
- Processes page: overview tiles (CPU, memory, disk, network, GPU) that jump to their Performance view,
  filter chips (All / Apps / Background / System) with counts, "Hide idle processes", collapsible groups,
  app icons/avatars, status badges and a detail pane for the selected process with live CPU, memory and disk graphs.
- Details page: optional process tree view.
- Properties window with tabs for General, Threads, Open files and Environment (with filter).
- Export the process list as CSV (saved to the Downloads folder).
- Hover any graph to see the exact value and how long ago it was measured.
- `VIGIL_SYS_ROOT` environment variable to point the sensor and battery readers at another sysfs tree (testing).

### Changed
- Redesigned interface: icon navigation rail with live CPU/memory summary, cleaner header with a single View menu,
  card-based Performance and Settings pages, refined light and dark palettes, status bar with icons.

### Performance
- The full process scan only runs at the update speed while the Processes, Details or Users page is visible;
  otherwise every fifth refresh (GPU usage keeps updating). Idle processes skip most per-process reads,
  GPU file-descriptor rescans are tiered, and per-page aggregates are cached per scan.
  In our measurements the sampling thread uses about 30 % less CPU with many processes.

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
