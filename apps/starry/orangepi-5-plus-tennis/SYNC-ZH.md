# orangepi-5-plus-tennis 同步说明

本文件 `apps/starry/orangepi-5-plus-tennis`。

## 应用简介

本应用是一个 StarryOS 板级网球捡球演示 + 基准测试程序，将 RK3588 网球捡球机器人工作流从参考仓库移植过来。电机和机械臂可以分别选择虚拟后端或兼容 `aka-rk3588` 接口的真实后端；默认使用虚拟后端，因此无需实体小车也可运行。

整个项目被定位为一次**端到端延迟（END-TO-END LATENCY）优化**：竞赛目标是最小化从摄像头取帧到电机/机械臂指令、再到完成捡球的时间。强制交付物是 `TENNIS_BENCH_RESULT` 基准测试输出行。

二进制为单个原生 Linux（aarch64-linux-gnu，glibc）C++ 程序，命名为 `tennis_app`，使用 CMake 交叉编译，运行在 StarryOS 下的板级 Linux rootfs 上。单一二进制通过 `--mode` 切换模式。

## 当前进度

现在已经可用（板子 + 摄像头，无小车）：

- UVC 采集
- MJPEG 解码
- RKNN YOLOv8 网球检测
- 完整状态机
- HSV 红色桶检测
- 可独立选择的虚拟/真实差速电机和夹爪机械臂后端
- RK3588 PWM sysfs + DRV8833 电机后端
- ESP32-C3 UART 电机后端与 ZP10S UART 舵机后端
- `TENNIS_BENCH_RESULT` 基准测试
- 可选的轮式里程计辅助返回桶区域（Linux live 配置已完成标定并启用）

后续：
- 将预处理切换到 RGA（待 `/dev/rga` 可用）
- 拆分独立控制线程 + 多个 `rknn_dup_context` NPU worker，实现真正的三核并行（多核多上下文 NPU）
- 增加 Linux 与 Starry 的基准对比

## 如何运行

### 主机（host）dry-run 自测

`dry-run` 模式不需要摄像头/模型/板子：它用确定性的合成场景驱动整个状态机，并输出 `TENNIS_BENCH_RESULT`。这是可在主机上复现的基准。

主机构建（`dry-run` 是该构建中唯一的模式）：

```
cmake -S tennis-app -B tennis-app/build-host -DTENNIS_HOST_DRYRUN=ON && cmake --build tennis-app/build-host
./tennis-app/build-host/tennis_app --mode dry-run --duration-sec 5
```

### 板级运行（xtask）

```
cargo xtask starry app board -t orangepi-5-plus-tennis [--board-config configs/board-orangepi-5-plus-bench.toml]
```

- 默认板级配置（`board-orangepi-5-plus.toml`）运行一个有界的 dry-run 冒烟测试（无需摄像头/模型——证明二进制 + 基准输出能在 StarryOS 上跑通）。
- `configs/board-orangepi-5-plus-bench.toml` 运行 60s 的 LIVE 感知基准测试（需要摄像头 + 模型）。
- `configs/board-orangepi-5-plus-long-run.toml` 无限期实时运行。

xtask 只部署 StarryOS 内核（KERNEL）；`tennis_app` 二进制 + 库 + 模型必须已经安装在板子 rootfs 上。

### 板上安装 tennis_app + 模型（板级运行的前置步骤，手动）

1. 在带有 aarch64-linux-gnu 工具链的 Linux 主机上用 `build-image-runner.sh` 构建。
2. 将 `install/rk3588_linux_aarch64/tennis_app/` 复制到板子的 `/tennis_app`（持有板子 lease 时通过 SSH rsync，然后 `chown root:root`，`sync`）。
3. 该目录必须包含 `tennis_app`、`lib/librknnrt.so`、`model/`、`validation/`。
4. 对于 LIVE 模式，板子上还必须有 libuvc（例如 `/usr/lib/aarch64-linux-gnu/libuvc.so`），并接好 UVC 摄像头。
5. `dry-run` 只需要二进制 + `lib/librknnrt.so`。

### 各模式命令（精确）

```
tennis_app --mode live --model model/tennis.rknn --label model/labels.txt --device 0 --width 640 --height 480 --fps 30 --duration-sec 60 --virtual-actuators
tennis_app --mode test-uvc --device 0
tennis_app --mode test-yolo --model model/tennis.rknn --device 0
tennis_app --mode test-bucket --device 0
tennis_app --mode dry-run --duration-sec 10 --virtual-actuators

# aka00v4-rk3588：ESP32-C3 UART 底盘 + ZP10S UART 机械臂
tennis_app --mode live --motor-backend uart --motor-device /dev/ttyS6 \
  --arm-backend uart --arm-device /dev/ttyS3
```

## 执行器选择

电机支持 `virtual`、`pwm` 和 `uart`，机械臂支持 `virtual` 和 `uart`，两者可独立组合。`--virtual-actuators` 保留为同时选择两个虚拟后端的兼容参数。真实 UART 机械臂按标定动作同步等待舵机到位，因此其动作耗时会计入 LIVE 模式延迟；真实后端初始化失败时程序直接报错退出，不会静默切换为虚拟后端。

LIVE 控制使用单调时钟推进抓取/投放前制动阶段，不依赖相机帧率，也不会用阻塞循环反复写执行器。真实机械臂在启动时归位；相机需在限定时间内产生连续有效帧，运行中停帧超过 watchdog 会停车退出；执行器 I/O 失败以及 `SIGINT`/`SIGTERM` 同样通过正常清理路径停车。`dry-run` 强制使用虚拟执行器，避免合成场景驱动物理硬件。

`aka00v4-rk3588` 的已验证接线为：ESP32-C3 底盘使用
`/dev/ttyS6`，ZP10S 机械臂使用 `/dev/ttyS3`，波特率均为 115200。
底盘使用 `0x13` 命令一次发送左右轮有符号百分比，只有初始化和配置命令等待
ACK。启用真实执行器前，必须先在设备树中启用并分别验证这两个 UART 节点及
pinmux；真实后端初始化失败时不会回退到虚拟后端。

里程计首版实现使用同一底盘 UART 后端串行查询左右轮 RPM，默认每 100 ms
采样并按差速模型积分。投球完成后重置桶锚点；下一次抓球后若里程数据有效，
进入 `RETURN_TO_BUCKET` 做粗略转向和接近。期间只要视觉检测到任意桶，就立即
刹车并交回 `FIND_BUCKET`；RPM 失效、超时、超距或进入安全半径也会回退到视觉
搜索。Linux live 配置使用已标定的 `0.03 m` 轮径和 `0.18 m` 有效轮距，并以
`odometry-enabled=true` 启用；完整返桶流程仍需实机验证和调整返回阈值。

## 产生的指标

基准输出行 `TENNIS_BENCH_RESULT` 各关键字段含义：

- `duration_sec` —— 本次运行时长（秒）。
- `captured` —— 采集到的帧数。
- `processed` —— 处理（经过感知 + 控制循环）的帧数。
- `detections` —— 网球检测命中总数。
- `bucket_detections` —— 桶检测命中总数。
- `virtual_motor_commands` —— 电机指令次数（字段名为兼容保留，真实后端同样计数）。
- `virtual_arm_commands` —— 机械臂指令次数（字段名为兼容保留，真实后端同样计数）。
- `frame_to_detection_ms_avg` / `frame_to_detection_ms_p50` / `frame_to_detection_ms_p95` —— 取帧到检测的延迟（capture->detection）的平均值 / 中位数 / p95。
- `frame_to_command_ms_avg` / `frame_to_command_ms_p50` / `frame_to_command_ms_p95` —— 取帧到指令的延迟（capture->command，即竞赛指标）的平均值 / 中位数 / p95。
- `decode_errors` —— 解码错误次数。
- `inference_errors` —— 推理错误次数。
- `memory_rss_kb` —— 常驻内存（RSS，单位 KB）。

说明：在 `dry-run` 中感知路径是合成的，因此 `frame_to_detection` 约等于 0，延迟只反映控制环路的开销；LIVE 数值才反映真实的 MJPEG 解码 + NPU 推理。

相关输出行（精确格式）还包括起始标记 `TENNIS_BENCH_BEGIN`、每帧 `TENNIS_STATE` 与 `TENNIS_CMD`、虚拟电机 `TENNIS_MOTOR drive/brake/standby`、虚拟机械臂 `TENNIS_ARM grab|release|ready`，以及结束标记 `TENNIS_BENCH_DONE`。测试模式还会打印 `TENNIS_TEST_UVC` / `TENNIS_TEST_YOLO` / `TENNIS_TEST_BUCKET` 及各自的 `_DONE`。

## 模型说明

`tennis.rknn` 是一个**有文档记录的占位（PLACEHOLDER）**，不随仓库分发。参考仓库 tennis-train 只提供了 PyTorch->ONNX 导出和一个 Sophgo cv181x cvimodel——**没有 .rknn**、**没有 rknn-toolkit2 步骤**、且**没有 LICENSE**（数据集是第三方 Roboflow 数据）。

模型为单类 YOLOv8n，输入 640x640，RGB，`/255` 归一化（mean=0，scale=1/255），输入排布 NCHW，单类 `tennis_ball`。`labels.txt` 只有一行：`tennis_ball`。

为 RK3588 生成 `tennis.rknn` 的方法：取 FP32 ONNX，用 rknn-toolkit2 执行 `rknn.config(mean_values=[[0,0,0]], std_values=[[255,255,255]], target_platform=rk3588)`、`rknn.load_onnx`、`rknn.build(do_quantization=True, dataset=<有效图片列表>)`、`rknn.export_rknn(tennis.rknn)`。复用的后处理解码的是 rknn_model_zoo 的三分支 YOLOv8 head，因此请按该布局（单类）导出模型。

**今日（无专用网球模型）即可使用**：将 `--model` 指向同级 COCO 模型（例如 `/rknn_yolov8_image/model/yolov8.rknn`），`--label` 指向其 coco 标签，并加 `--ball-class 32`（sports ball / 运动球）。

若运行时模型路径缺失，应用会打印 `rknn_init fail!` 并以非零状态退出。

## 署名与许可

本工作流移植自 pengzechen 的参考仓库 aka-rk3588（https://github.com/pengzechen/aka-rk3588）和 tennis-train（https://github.com/pengzechen/tennis-train）。

这些参考仓库**没有 LICENSE**，因此本应用中的状态机、转向数学、HSV 桶检测器和执行器后端都是基于文档化行为的**干净重写（CLEAN REIMPLEMENTATIONS）**，并非复制的代码。

复用的 RKNN/UVC/图像工具代码是 **Apache-2.0（Rockchip）**，从同级的 orangepi-5-plus-uvc-rknn 应用共享（`TENNIS_RKNN_SHARED_DIR`，默认 `../../orangepi-5-plus-uvc-rknn/rknn-yolov8-image`），多 MB 的库和模型**不复制**。

模型权重/数据集来源**未决**（无 license + 第三方 Roboflow 数据）；在把任何 `tennis.rknn` 打包进仓库之前必须先厘清。
