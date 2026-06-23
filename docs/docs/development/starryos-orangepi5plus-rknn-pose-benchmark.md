---
sidebar_position: 32
sidebar_label: "Orange Pi 5 Plus RKNN Pose 性能记录"
---

# Orange Pi 5 Plus RKNN Pose 性能记录

本文记录在 Orange Pi 5 Plus / RK3588 上运行 MediaPipe Pose RKNN
测试程序时，Linux 官方运行环境和 StarryOS 当前 RKNN 兼容路径的
端到端指标对比。

这不是严格性能评测，只是当前移植阶段的参考基线。它用于判断：

- RKNN 模型和测试资产是否能在板端正确运行。
- StarryOS 的 RKNN ioctl / NPU 驱动路径相对 Linux 官方驱动还有多大差距。
- 后续优化时是否真的改善了 `inputs_set_ms`、`run_ms`、`outputs_get_ms`。

## 测试对象

测试目录：

```text
/rknn_mediapipe_pose_image
```

测试程序：

```text
/rknn_mediapipe_pose_image/rknn_pose_image
```

模型文件：

```text
/rknn_mediapipe_pose_image/model/pose_detector.rknn
/rknn_mediapipe_pose_image/model/pose_landmark_lite.rknn
```

测试图片：

```text
/rknn_mediapipe_pose_image/validation/person.jpg
```

测试命令：

```bash
cd /rknn_mediapipe_pose_image
./rknn_pose_image \
  --detector-model model/pose_detector.rknn \
  --landmark-model model/pose_landmark_lite.rknn \
  --dump-output-stats validation/person.jpg
```

## 指标含义

`rknn_pose_image` 当前会对 detector 和 landmark 两个 RKNN 模型分别输出
`POSE_RUN_RESULT`：

```text
POSE_RUN_RESULT model=<name> inputs_set_ms=<ms> run_ms=<ms> outputs_get_ms=<ms> outputs=<n>
```

字段含义：

- `inputs_set_ms`：准备输入并调用 RKNN runtime 设置输入的耗时。
- `run_ms`：调用 `rknn_run()` 触发 NPU 推理并等待完成的耗时。
- `outputs_get_ms`：调用 `rknn_outputs_get()` 取回输出 tensor 的耗时。
- `outputs`：模型输出 tensor 数量。

注意：`inputs_set_ms` 包含当前测试程序里的 resize / letterbox 和输入设置路径，
不是纯 NPU 推理耗时。更接近纯推理阶段的是 `run_ms`。

## Linux 官方运行环境结果

板子 Linux 环境：

```text
Orange Pi 1.2.0 Jammy
Linux 6.1.43-rockchip-rk3588
```

关键输出：

```text
POSE_LETTERBOX model=detector x_pad=54 y_pad=0 input=324x617 resize=116x224 scale=0.36304700 ms=1.47
POSE_RUN_RESULT model=detector inputs_set_ms=8.05 run_ms=13.56 outputs_get_ms=0.12 outputs=2

POSE_LETTERBOX model=landmark x_pad=62 y_pad=0 input=324x617 resize=132x256 scale=0.41491085 ms=1.10
POSE_RUN_RESULT model=landmark inputs_set_ms=3.63 run_ms=36.37 outputs_get_ms=1.97 outputs=5

POSE_IMAGE_PROBE_DONE
```

Linux 侧结果汇总：

| 模型 | inputs_set_ms | run_ms | outputs_get_ms | outputs |
| --- | ---: | ---: | ---: | ---: |
| detector | 8.05 | 13.56 | 0.12 | 2 |
| landmark | 3.63 | 36.37 | 1.97 | 5 |

## StarryOS 当前结果

StarryOS 启动环境：

```text
arch = aarch64
platform = aarch64-generic
target = aarch64-unknown-none-softfloat
smp = 1
```

关键输出：

```text
POSE_LETTERBOX model=detector x_pad=54 y_pad=0 input=324x617 resize=116x224 scale=0.36304700 ms=4.72
POSE_RUN_RESULT model=detector inputs_set_ms=67.96 run_ms=145.22 outputs_get_ms=1.63 outputs=2

POSE_LETTERBOX model=landmark x_pad=62 y_pad=0 input=324x617 resize=132x256 scale=0.41491085 ms=5.52
POSE_RUN_RESULT model=landmark inputs_set_ms=29.37 run_ms=58.46 outputs_get_ms=22.22 outputs=5

POSE_IMAGE_PROBE_DONE
POSE_IMAGE_DONE
```

StarryOS 侧结果汇总：

| 模型 | inputs_set_ms | run_ms | outputs_get_ms | outputs |
| --- | ---: | ---: | ---: | ---: |
| detector | 67.96 | 145.22 | 1.63 | 2 |
| landmark | 29.37 | 58.46 | 22.22 | 5 |

## 对比

| 模型 | 指标 | Linux | StarryOS | StarryOS / Linux |
| --- | --- | ---: | ---: | ---: |
| detector | inputs_set_ms | 8.05 | 67.96 | 8.44x |
| detector | run_ms | 13.56 | 145.22 | 10.71x |
| detector | outputs_get_ms | 0.12 | 1.63 | 13.58x |
| landmark | inputs_set_ms | 3.63 | 29.37 | 8.09x |
| landmark | run_ms | 36.37 | 58.46 | 1.61x |
| landmark | outputs_get_ms | 1.97 | 22.22 | 11.28x |

当前现象：

- 两侧输出 tensor 的统计值一致，说明模型、输入图片、预处理和 RKNN 输出路径
  在功能上已经对齐。
- Linux 官方驱动下 detector `run_ms` 为 `13.56 ms`，StarryOS 当前为
  `145.22 ms`，这是最明显的性能差距。
- landmark 的 `run_ms` 差距较小，但 `outputs_get_ms` 在 StarryOS 侧明显更高。
- StarryOS 日志中 rknpu submit 路径还存在较多兼容层日志和同步开销，当前结果
  更适合作为移植阶段基线，不应直接视为最终性能。

## 后续优化方向

优先关注：

1. `rknn_run()` 对应的 rknpu submit / wait 路径。
2. output buffer 同步与拷贝路径，尤其是 landmark `outputs_get_ms`。
3. input buffer 设置路径，确认 StarryOS 侧是否有额外拷贝或 cache sync 开销。
4. 减少调试日志对端到端耗时的干扰后，再重复采样。

每次优化后建议重复执行同一条命令，并保留新的 `POSE_RUN_RESULT` 行。
