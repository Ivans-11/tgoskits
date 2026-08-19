# orangepi-5-plus-tennis

A StarryOS board demo and benchmark app that ports the RK3588 tennis-ball pickup robot workflow to an OrangePi-5-Plus (RK3588) + UVC camera. Motor and arm backends are selected independently, with virtual backends for reproducible benchmarks and real backends compatible with `aka-rk3588` hardware.

## What this demonstrates

This app drives the full tennis pickup workflow — chase a tennis ball, grab it, find a red bucket, approach it, and deposit — and frames it as an **end-to-end latency benchmark**. The competition goal is minimizing the time from a camera frame to the motor/arm command that acts on it (frame-to-command latency), and the mandatory deliverable is the `TENNIS_BENCH_RESULT` line. Perception (YOLOv8 ball detection + HSV bucket detection) and control (a differential-drive state machine) run on the board's NPU and CPU. Virtual actuators keep the benchmark reproducible; real actuators execute the same control commands on the robot.

## Actuator backends

The default virtual motor/arm backends give a clean, **non-blocking** seam between the control loop and the hardware:

- It **runs with no physical car** — only the OrangePi-5-Plus board and a UVC camera (for live modes) are required.
- Because the virtual actuator backend never blocks, **frame-to-command latency stays pure compute** — there is no UART round-trip or servo settle time inflating the benchmark.
- The motor can instead use four RK3588 PWM sysfs channels driving a DRV8833, or the ESP32-C3 UART controller used by `aka-rk3588`.
- The arm can instead use the ZP10S UART bus-servo controller and the calibrated `aka00v4-rk3588` action sequence.
- Motor and arm selection is independent, so mixed configurations such as a PWM chassis with a virtual arm are supported. Real backend initialization errors are fatal and never silently fall back to virtual output.
- `dry-run` always requires both virtual backends, so a synthetic scene cannot accidentally move physical hardware.

## Architecture

The pipeline is built for low frame-to-command latency:

- **Capture thread (drop-old):** `libuvc` runs its own capture thread and latches only the **newest** frame into a sequenced latest-frame slot (frame id + capture timestamp). Old frames are dropped rather than queued.
- **Latest-frame slot:** the perception/control loop reads the most recent frame from this slot, so it never works on stale backlog.
- **Perception + control loop:** reads the latest frame, runs YOLOv8 **or** HSV per the current state (never both — no redundant decode), runs the state machine, and calls the selected actuator backends. Virtual backends are non-blocking; UART initialization ACK waits and calibrated servo movement delays are included in live latency when real UART hardware is selected.
- **Timed motion phases:** grab/deposit braking uses monotonic deadlines rather than blocking loops or frame counts. A 1 ms control tick advances these deadlines even without a new frame, while command de-duplication avoids repeated UART/sysfs writes.
- **Fail-safe lifecycle:** a real arm is homed before control starts, startup waits for consecutive valid camera frames, a no-frame watchdog stops the run, actuator I/O failures are fatal, and `SIGINT`/`SIGTERM` return through normal motor shutdown.
- **Multi-core NPU:** inference runs across all three RK3588 NPU cores via `rknn_set_core_mask` (`RKNN_NPU_CORE_0_1_2`). The stock image/stream binaries do not set this.
- **Optional odometry return:** the UART chassis can query wheel RPM through the
  same serialized backend and guide the post-grab return toward the last bucket
  anchor. Bucket vision takes over immediately whenever a bucket is detected.
- **Single-class postprocess:** the YOLOv8 head is decoded for a single tennis-ball class. No per-frame heap allocation in the steady state.
- **HSV bucket detection:** the bucket is found with an HSV red-bucket detector rather than a second NPU pass.

## Reuse (no duplication)

The board build **reuses** the vendored, Apache-2.0 (Rockchip) RKNN/UVC code from the sibling app `apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image` instead of copying it. Via the CMake cache variable `TENNIS_RKNN_SHARED_DIR` (default `../../orangepi-5-plus-uvc-rknn/rknn-yolov8-image`) it pulls in:

- the sibling's `3rdparty/` libraries (`librknnrt.so`, `librga`, `libturbojpeg`, `stb_image`, rknn headers),
- its `utils/` (`image_utils`, `image_drawing`, `file_utils`),
- and its `cpp/` modules (`uvc_capture`, `rknpu2/yolov8`, `postprocess`).

The multi-MB libraries and the model are **not** copied. The tennis-specific code — state machine, `tennis_detector` wrapper, HSV `bucket_detector`, actuator backends, metrics, `main`, and the live pipeline — is new and lives under `tennis-app/cpp`.

## Build

### Host self-test (dry-run, no board/camera/model)

```bash
cmake -S tennis-app -B tennis-app/build-host -DTENNIS_HOST_DRYRUN=ON
cmake --build tennis-app/build-host
./tennis-app/build-host/tennis_app --mode dry-run --duration-sec 5
```

`-DTENNIS_HOST_DRYRUN=ON` produces a self-contained native build with no RKNN/UVC/RGA dependency; `--mode dry-run` is the only mode in that build. It drives the full state machine from a deterministic synthetic scene and emits `TENNIS_BENCH_RESULT`, so the benchmark is reproducible on any host (and in CI).

### Board cross-build

Install an aarch64 Linux **glibc** cross toolchain (the prebuilt Rockchip libs are glibc — do **not** use a musl toolchain):

```bash
# Linux / tgoskits container
sudo apt-get install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu
# macOS (Homebrew)
brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
```

Then:

```bash
./build-image-runner.sh
```

`build-image-runner.sh` auto-detects the cross prefix (`aarch64-linux-gnu-`, else `aarch64-unknown-linux-gnu-`; override with `CROSS_COMPILE=`). It builds with `CMAKE_SYSTEM_NAME=Linux`, `TARGET_SOC=rk3588`, Release. The output lands in:

```
tennis-app/install/rk3588_linux_aarch64/tennis_app/
```

containing the `tennis_app` binary + `lib/librknnrt.so` + `model/` + `validation/`. `libstdc++`/`libgcc` are statically linked so the binary does not depend on the board's C++ runtime version; glibc stays dynamic (the board's glibc must be ≥ the toolchain's — use the container toolchain if you hit a `GLIBC_…` runtime error). The binary's only bundled runtime dependency is `librknnrt.so` (via `RPATH=$ORIGIN/lib`); `libuvc` is `dlopen`'d at runtime for live modes only.

## Run on the board

Runtime options can be loaded from a `key=value` file. Keys match the long CLI
options without the leading `--`; blank lines and lines beginning with `#` or
`;` are ignored. Command-line options override the file, regardless of where
`--config` appears:

```bash
./tennis_app --config configs/orangepi5plus-live.conf
./tennis_app --config configs/orangepi5plus-live.conf --duration-sec 60
```

The supplied configuration selects `/dev/ttyS6` for the chassis and
`/dev/ttyS3` for the arm. It runs for 600 seconds so `Ctrl-C` can still stop the
program through its normal motor-shutdown path.

On the Orange Pi, build the self-contained board binary with the vendored
RKNN/RGA/MPP libraries:

```bash
./build-native.sh
```

Run through xtask:

```bash
# Default: bounded dry-run smoke (no camera/model needed)
cargo xtask starry app board -t orangepi-5-plus-tennis

# Live 60s perception benchmark (camera + model required)
cargo xtask starry app board -t orangepi-5-plus-tennis --board-config configs/board-orangepi-5-plus-bench.toml

# Live indefinitely
cargo xtask starry app board -t orangepi-5-plus-tennis --board-config configs/board-orangepi-5-plus-long-run.toml
```

- The **default** board config (`board-orangepi-5-plus.toml`) runs a bounded **dry-run smoke** — no camera or model needed. It proves the binary runs and emits the benchmark output on real StarryOS.
- `configs/board-orangepi-5-plus-bench.toml` runs the **60s live** perception benchmark (needs camera + model).
- `configs/board-orangepi-5-plus-long-run.toml` runs live indefinitely.

**The xtask deploys the StarryOS kernel only.** The `tennis_app` binary + libs + model must already be installed on the board rootfs (see below).

## On-board install

The board runs above are gated on a manual install:

1. Build with `./build-image-runner.sh` on a Linux host.
2. Copy `install/rk3588_linux_aarch64/tennis_app/` to the board at `/tennis_app` (rsync over SSH while holding a board lease), then `chown root:root` and `sync`.
3. `/tennis_app` must contain: `tennis_app`, `lib/librknnrt.so`, `model/`, `validation/`.

For **live** modes, `libuvc` must also be present on the board (e.g. `/usr/lib/aarch64-linux-gnu/libuvc.so`) and a UVC camera connected. **dry-run** needs only the binary + `lib/librknnrt.so`.

## Required runtime assets

- **`model/tennis.rknn` — documented placeholder, not shipped.** The `tennis-train` reference repo ships only a PyTorch→ONNX export and a Sophgo cv181x cvimodel — there is no `.rknn`, no rknn-toolkit2 step, and no LICENSE (the dataset is third-party Roboflow data). The model is a single-class YOLOv8n at 640x640, RGB, `/255` normalization (mean=0, scale=1/255), input NCHW, single class `tennis_ball`.

  To produce `tennis.rknn` for RK3588: take the FP32 ONNX, then use rknn-toolkit2:
  ```python
  rknn.config(mean_values=[[0,0,0]], std_values=[[255,255,255]], target_platform='rk3588')
  rknn.load_onnx(...)
  rknn.build(do_quantization=True, dataset=<list of valid images>)
  rknn.export_rknn('tennis.rknn')
  ```
  The reused postprocess decodes the `rknn_model_zoo` 3-branch YOLOv8 head, so export the model in that layout (single class).

- **COCO fallback (works today, no dedicated tennis model):** point `--model` at the sibling COCO model (e.g. `/rknn_yolov8_image/model/yolov8.rknn`), pass its COCO labels via `--label`, and set `--ball-class 32` (COCO sports ball).

- **`model/labels.txt`** — one line: `tennis_ball` (for the single-class tennis model). For the COCO fallback, use the COCO labels file instead.

> If the model path is missing at runtime the app prints `rknn_init fail!` and exits non-zero.

## Modes

```bash
tennis_app --mode live --model model/tennis.rknn --label model/labels.txt --device 0 --width 640 --height 480 --fps 30 --duration-sec 60 --virtual-actuators
tennis_app --mode test-uvc --device 0
tennis_app --mode test-yolo --model model/tennis.rknn --device 0
tennis_app --mode test-bucket --device 0
tennis_app --mode dry-run --duration-sec 10 --virtual-actuators

# aka00v4-rk3588: ESP32-C3 UART chassis + ZP10S UART arm
tennis_app --mode live --motor-backend uart --motor-device /dev/ttyS6 \
  --arm-backend uart --arm-device /dev/ttyS3
```

`dry-run` needs no camera/model/board and emits `TENNIS_BENCH_RESULT`. The test modes additionally print `TENNIS_TEST_UVC ...` + `TENNIS_TEST_UVC_DONE`, `TENNIS_TEST_YOLO ...` + `TENNIS_TEST_YOLO_DONE`, and `TENNIS_TEST_BUCKET ...` + `TENNIS_TEST_BUCKET_DONE`.

For output compatibility, the historical `virtual_motor_commands` and `virtual_arm_commands` result fields count commands sent through the selected backends, including real backends.

## CLI flags

| Flag | Default | Meaning |
|------|---------|---------|
| `--config <path>` | — | Load `key=value` options; CLI options override the file |
| `--mode <live\|test-uvc\|test-yolo\|test-bucket\|dry-run>` | — | Run mode |
| `--model <path>` | `model/tennis.rknn` | RKNN model path |
| `--label <path>` | `model/labels.txt` | Labels file |
| `--device <n>` | `0` | UVC device index |
| `--width <n>` / `--height <n>` | `640` / `480` | Capture resolution |
| `--fps <n>` | `30` | Capture frame rate |
| `--duration-sec <f>` | `60` | Run duration in seconds |
| `--motor-backend <virtual\|pwm\|uart>` | `virtual` | Chassis backend |
| `--motor-device <spec>` | UART: `/dev/ttyS6` | Four PWM chip paths or motor UART path |
| `--arm-backend <virtual\|uart>` | `virtual` | Arm backend |
| `--arm-device <path>` | `/dev/ttyS3` | ZP10S UART path |
| `--grab-motion-ms <n>` | `300` | Servo movement time for each grab step |
| `--motor-min-speed <n>` | `20` | Minimum common translation speed before steering bias |
| `--area-far <f>` | `0.20` | Ball area ratio below which far approach speed is used |
| `--area-stop <f>` | `0.28` | Ball area ratio that stops the approach |
| `--area-reverse <f>` | `0.50` | Ball area ratio that triggers reverse |
| `--stop-center-offset <n>` | `90` | Gripper target offset from image center in pixels |
| `--stop-center-zone <n>` | `20` | Horizontal alignment tolerance in pixels |
| `--stop-confirm-cnt <n>` | `3` | Consecutive close and aligned frames required before grab |
| `--chase-far-speed <n>` | `45` | Far-distance ball approach speed |
| `--chase-forward-speed <n>` | `30` | Near-distance ball approach speed |
| `--chase-pivot-speed <n>` | `30` | Fixed ball alignment rotation speed |
| `--reverse-speed <n>` | `30` | Fixed too-close reverse speed |
| `--search-pivot-speed <n>` | `30` | Fixed ball search rotation speed |
| `--odometry-enabled <bool>` | `false` | Enable RPM odometry return guidance |
| `--odometry-sample-ms <n>` | `100` | Wheel-RPM sampling interval |
| `--return-timeout-ms <n>` | `15000` | Maximum blind return duration |
| `--return-stop-radius <f>` | `0.50` | Stop radius before visual bucket search |
| `--bucket-min-area <n>` | `3000` | Minimum connected red pixels required to detect a bucket |
| `--bucket-area-brake <f>` | `0.70` | Bucket red-area ratio that selects near approach speed |
| `--bucket-area-deposit <f>` | `0.90` | Bucket red-area ratio that triggers deposit |
| `--virtual-actuators` | — | Compatibility shorthand selecting both virtual backends |
| `--camera-warmup-frames <n>` | `3` | Consecutive valid frames required before live control |
| `--camera-warmup-timeout-ms <n>` | `3000` | Camera startup deadline |
| `--camera-watchdog-ms <n>` | `2000` | Stop after this long without a new frame |
| `--ball-class <n>` | `0` | `0` for a single-class tennis model; `32` = COCO sports ball |
| `--min-confidence <0-100>` | `50` | Detection confidence threshold |
| `--log-every <n>` | `1` | Emit per-frame lines every Nth frame |
| `--staleness-ms <n>` | `0` | `0` = never drop on age |
| `--core-mask <m>` | `all` | One of `all\|auto\|0\|1\|2\|0_1\|0_1_2` |

## Example output

A 5s host dry-run. In dry-run the perception path is synthetic, so `frame_to_detection` is ~0 and the latencies reflect control-loop plumbing only — **live** numbers reflect real MJPEG decode + NPU inference.

```
TENNIS_BENCH_BEGIN mode=dry-run fps=30 duration_sec=5.000 virtual_actuators=1
TENNIS_STATE frame=0 state=CHASE_BALL detections=1 bucket_visible=0 frame_age_ms=0.000
TENNIS_CMD frame=0 state=CHASE_BALL motor_left=41 motor_right=31 arm_action=none capture_ts_ns=24151827553000 decision_ts_ns=24151827566000 frame_to_command_ms=0.013
TENNIS_BENCH_RESULT duration_sec=5.003 captured=150 processed=150 detections=98 bucket_detections=42 virtual_motor_commands=76 virtual_arm_commands=5 frame_to_detection_ms_avg=0.000 frame_to_detection_ms_p50=0.000 frame_to_detection_ms_p95=0.001 frame_to_command_ms_avg=0.003 frame_to_command_ms_p50=0.002 frame_to_command_ms_p95=0.006 decode_errors=0 inference_errors=0 memory_rss_kb=1360
TENNIS_BENCH_DONE
```

The full set of verbatim output-line formats:

```
TENNIS_BENCH_BEGIN mode=<...> ...
TENNIS_STATE frame=<id> state=<CHASE_BALL|GRAB|FIND_BUCKET|APPROACH_BUCKET|DEPOSIT> detections=<n> bucket_visible=<0|1> frame_age_ms=<...>
TENNIS_CMD frame=<id> state=<...> motor_left=<l> motor_right=<r> arm_action=<none|grab|release|ready> capture_ts_ns=<...> decision_ts_ns=<...> frame_to_command_ms=<...>
TENNIS_MOTOR drive left=<l> right=<r>      (also: TENNIS_MOTOR brake / TENNIS_MOTOR standby) — emitted only when the command changes
TENNIS_ARM grab|release|ready              — emitted by the virtual arm backend
TENNIS_BENCH_RESULT ...
TENNIS_BENCH_DONE
```

## Benchmark fields

Fields on the `TENNIS_BENCH_RESULT` line:

- `duration_sec` — measured run duration in seconds.
- `captured` — frames latched by the capture thread.
- `processed` — frames the perception/control loop actually ran on.
- `detections` — frames in which a ball was detected.
- `bucket_detections` — frames in which the red bucket was detected.
- `virtual_motor_commands` — distinct motor commands emitted (command changed).
- `virtual_arm_commands` — arm commands emitted (grab/release/ready).
- `frame_to_detection_ms_avg` / `_p50` / `_p95` — capture→detection latency (avg, median, 95th percentile).
- `frame_to_command_ms_avg` / `_p50` / `_p95` — capture→command latency, the competition metric (avg, median, 95th percentile).
- `decode_errors` — MJPEG decode failures.
- `inference_errors` — NPU inference failures.
- `memory_rss_kb` — process resident set size in KB.

`frame_to_detection` is the capture→detection latency; `frame_to_command` is the capture→command latency (the competition metric).

## Works now (board + camera) vs requires the physical car later

**Works now (board + camera, no car):**

- UVC capture
- MJPEG decode
- RKNN YOLOv8 ball detection
- Full state machine (`CHASE_BALL`, `GRAB`, `RETURN_TO_BUCKET`, `FIND_BUCKET`, `APPROACH_BUCKET`, `DEPOSIT`)
- HSV red-bucket detection
- Selectable virtual/PWM/UART motor and virtual/UART arm backends
- The `TENNIS_BENCH_RESULT` benchmark

**Real actuator support (explicitly selected, not the default):**

- RK3588 PWM sysfs + DRV8833 differential-motor backend
- ESP32-C3 UART differential-motor backend (`0xAA 0x55` framed protocol @115200)
- ZP10S UART gripper-arm backend (ASCII `#IDPpulseTtime!` @115200)

The known working `aka00v4-rk3588` wiring is `/dev/ttyS6` for the
ESP32-C3 chassis and `/dev/ttyS3` for the ZP10S arm, both at 115200 baud.
The chassis sends paired signed percentage values with command `0x13`; only
initialization and configuration wait for ACKs. Before selecting real
actuators, both UART nodes and their pinmux must be enabled in the board device
tree and verified independently. The application never falls back from a
failed real backend to a virtual backend.

## Attribution

The workflow is ported from the reference repos [aka-rk3588](https://github.com/pengzechen/aka-rk3588) and [tennis-train](https://github.com/pengzechen/tennis-train) by **pengzechen**. Those repos carry **no license**, so the state machine, steering math, HSV bucket detector, and actuator backends here are **clean reimplementations** from the documented behaviour, not copied code.

The reused RKNN/UVC/image-utility code is Apache-2.0 (Rockchip) and is shared from the sibling `orangepi-5-plus-uvc-rknn` app (no duplication).

**Model/dataset provenance is unresolved** (no license + third-party Roboflow data). Clear it before bundling any `tennis.rknn` into the repo.

## Next steps

1. Move the calibrated multi-step arm sequence to an asynchronous executor if perception must continue while the chassis is intentionally stopped for grabbing.
2. Evaluate multiple `rknn_dup_context` NPU workers against the current low-latency latest-frame pipeline.
3. Add a Linux-vs-Starry benchmark comparison.
