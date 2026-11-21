# Benchmarks & Resource Measurements

Reproducible idle resource, startup, and binary footprint measurements comparing **QuantumView (Servo 0.8.0)** against **Windows WebView2 (`wry 0.57.1`)** on the identical comparison UI (`ui/index.html`, 1000×720 logical pixels).

## 2026-11-20 — Servo 0.8.0 vs WebView2 (`wry 0.57.1`)

### 1. Idle Resource Usage (3 runs × 30 s, 10 s warmup)

| Runtime | Processes | Private Bytes (Median) | Working Set Total (Median) | Idle CPU (1-Core %) | GPU Dedicated | GPU Shared |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| **QuantumView (Servo 0.8.0)** | **1** | 261.27 MiB | **112.32 MiB** | **0.000%** | 123.39 MiB | 15.68 MiB |
| WebView2 (`wry 0.57.1`) | 7 | 142.56 MiB | 314.75 MiB | 0.411% | 24.74 MiB | 1.84 MiB |

- Servo 0.8.0 runs inside a single process with lower total Working Set (`112.32` vs `314.75 MiB`) and `0.000%` idle CPU.
- WebView2 has lower Private Bytes (`142.56` vs `261.27 MiB`) and GPU Dedicated memory (`24.74` vs `123.39 MiB`), relying on the shared OS EdgeWebView2 Runtime across 7 processes.

### 2. Binary Footprint ([footprint.json](2026-11-20/footprint.json))

| Runtime | Executable | Runtime Dependency |
| --- | ---: | --- |
| **QuantumView (Servo 0.8.0)** | 111.94 MiB (`117,377,024` B) | Self-contained, single process, no OS WebView required |
| WebView2 (`wry 0.57.1`) | 1.00 MiB (`1,044,480` B) | Requires OS EdgeWebView2 Runtime (tested with `157.0.4341.60`) |

### 3. Warm Startup & End-to-End Scroll Self-Test ([startup.csv](2026-11-20/startup.csv))

- `--probe --screenshot` (warm launch through first rendered frame, PNG readback, disk write, exit — 3-run median): **1019.2 ms** (`1026.0` / `1012.2` / `1019.2 ms`)
- `--probe --self-test scroll` (launch + 3-step wheel interpolation + DOM check + exit — 3-run median): **3039.6 ms** (`3047.0` / `3039.1` / `3039.6 ms`)

### Test Environment ([environment.json](2026-11-20/environment.json))

- OS: Windows 10 Home (`10.0.19045`)
- CPU: Intel Core i7-4790 (4 cores / 8 threads)
- GPU: NVIDIA GeForce GTX 1070 Ti (driver `32.0.15.6094`)
- WebView2 Runtime: `157.0.4341.60`
- Raw data: [medians.csv](2026-11-20/medians.csv) · [summary.csv](2026-11-20/summary.csv) · [startup.csv](2026-11-20/startup.csv) · [binaries.json](2026-11-20/binaries.json)

---

## 2026-11-08 — Initial Baseline (Servo 0.7.0 / Blitz / WebView2)

Archived early baseline prior to the Servo 0.8.0 upgrade. Blitz / Dioxus Native was dropped from scope because it does not execute standard local HTML/JavaScript, and is kept here only for historical comparison.

| Runtime | Private MiB | Working Set Total MiB | Idle CPU (1-Core %) | GPU Dedicated MiB | GPU Shared MiB | Processes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Servo 0.7.0 | 261.434 | 111.691 | 0.000 | 123.387 | 15.680 | 1 |
| Blitz / Dioxus Native 0.7.10 | 404.469 | 150.434 | 0.000 | 261.906 | 49.156 | 1 |
| WebView2 (`wry 0.57.1`) | 144.496 | 295.695 | 0.151 | 24.738 | 1.840 | 7 |

Raw data: [2026-11-08/summary.csv](2026-11-08/summary.csv) · [2026-11-08/binaries.json](2026-11-08/binaries.json) · [2026-11-08/inputs.json](2026-11-08/inputs.json). See [docs/comparison.md](../docs/comparison.md) for measurement procedures.
