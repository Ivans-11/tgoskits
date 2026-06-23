# Orange Pi 5 Plus MediaPipe Pose 小方案

## 目标

在 Orange Pi 5 Plus + StarryOS 上先跑通一个 MediaPipe Pose 风格的人体姿态 demo：

1. 从 UVC 摄像头采集 MJPEG 帧。
2. 解码为 RGB 图像。
3. 使用 RKNN 在 NPU 上执行人体姿态推理。
4. 输出人体框和关键点结果。
5. 在原始帧上绘制人体骨架。
6. 通过现有 HTTP MJPEG 流预览标注结果。
7. 提供有限时长的 StarryOS board smoke test。

第一版目标是“能稳定跑起来并可观察”，不是一次性做完整 MediaPipe Framework 移植。

## 非目标

第一版暂不做：

- 完整 MediaPipe graph/runtime 移植；
- 多人姿态的复杂关联与跟踪；
- segmentation mask；
- 动作识别或手势/行为分类；
- 长时间性能压测；
- 复杂的 ROI tracking 和时序平滑。

这些功能等基础 detector + landmark 链路稳定后再补。

## 总体方案

采用 MediaPipe Pose/BlazePose 类似的两阶段链路：

1. **Pose detector**
   - 输入整帧图像。
   - 输出人体 ROI、置信度和必要的旋转/尺度信息。

2. **Pose landmark**
   - 输入 detector 得到的人体 ROI。
   - 输出人体关键点。
   - 第一版按 33 个 landmark 处理，每个点保留 `x/y/z/visibility/presence` 中模型实际提供的字段。

3. **坐标映射**
   - detector 输出的 ROI 位于模型输入坐标系。
   - landmark 输出位于 ROI 或模型输入坐标系。
   - 后处理负责把关键点映射回摄像头原图坐标。

4. **可视化**
   - 复用现有 `image_drawing` 绘制关键点和骨架线段。
   - 复用现有 stream demo 的 MJPEG 发布逻辑。

## 代码组织

建议不改现有 YOLOv8 检测路径，pose 相关模块使用独立目录：

```text
apps/starry/orangepi-5-plus-uvc-rknn/
└── rknn-mediapipe-pose-image/
    ├── cpp/
    │   ├── pose_model.h
    │   ├── pose_model.cc
    │   ├── pose_postprocess.h
    │   ├── pose_postprocess.cc
    │   ├── pose_stream_main.cc
    │   └── pose_bench_main.cc
    └── model/
        ├── pose_detector.rknn
        └── pose_landmark_lite.rknn
```

如果后续 pose demo 变大，还可以再独立成单独 Starry app：

```text
apps/starry/orangepi-5-plus-mediapipe-pose/
```

第一版先放在现有 UVC + RKNN app 内的 sibling 目录，减少重复搬运 JPEG、RKNN runtime
和 board 配置，同时避免把 MediaPipe pose 源码混进 YOLO 工程。

## 数据结构草案

```cpp
#define POSE_LANDMARK_NUM 33
#define POSE_MAX_PERSONS 1

typedef struct {
    float x;
    float y;
    float z;
    float visibility;
    float presence;
} pose_landmark_t;

typedef struct {
    image_rect_t box;
    float score;
    int landmark_count;
    pose_landmark_t landmarks[POSE_LANDMARK_NUM];
} pose_result_t;

typedef struct {
    int count;
    pose_result_t results[POSE_MAX_PERSONS];
} pose_result_list_t;
```

## 第一版执行流程

`rknn_pose_stream` 主流程：

1. 初始化 UVC 摄像头。
2. 初始化 detector RKNN context。
3. 初始化 landmark RKNN context。
4. 启动 MJPEG HTTP publisher。
5. 循环取最新帧：
   - MJPEG decode；
   - detector preprocess；
   - detector inference；
   - detector postprocess，取最高置信度人体；
   - ROI crop/resize；
   - landmark inference；
   - landmark postprocess；
   - 坐标映射回原图；
   - 绘制骨架；
   - 发布 annotated JPEG；
   - 打印 `POSE_RESULT`。
6. 达到 `--duration-sec` 或 `--max-inferences` 后退出。
7. 打印 `UVC_POSE_STREAM_DONE`。

## 命令行接口

建议新增：

```bash
./rknn_pose_stream \
  --detector-model model/pose_detector.rknn \
  --landmark-model model/pose_landmark_lite.rknn \
  --device 0 \
  --width 320 \
  --height 240 \
  --fps 30 \
  --duration-sec 30 \
  --infer-every 2 \
  --max-inferences 0 \
  --min-detection-confidence 50 \
  --min-landmark-confidence 50 \
  --http-port 8080 \
  --http-fps 10 \
  --jpeg-quality 75
```

固定图片调试入口：

```bash
./rknn_pose_image \
  --detector-model model/pose_detector.rknn \
  --landmark-model model/pose_landmark_lite.rknn \
  validation/person.jpg
```

## StarryOS Board Smoke

新增 board config 时，先做短跑：

```toml
shell_init_cmd = '''
echo UVC_POSE_STREAM_BEGIN && cd /rknn_yolov8_image && \
./rknn_pose_stream \
  --fps 10 \
  --infer-every 4 \
  --duration-sec 20 \
  --max-inferences 3 \
  --min-detection-confidence 50 \
  --min-landmark-confidence 50 \
  --http-port 8080 \
  --http-fps 2 \
  --jpeg-quality 70 && \
echo UVC_POSE_STREAM_DONE
'''

success_regex = [
  "(?m)^UVC_POSE_STREAM_DONE$",
]

fail_regex = [
  "(?i)\\bpanic(?:ked)?\\b",
  "(?i)segmentation fault",
  "(?i)error while loading shared libraries",
  "(?m)^rknn_init fail!.*$",
  "(?m)^(uvc_init|uvc_open|uvc_start_streaming|uvc_get_device_list) failed: .*$",
  "(?i)(rknn_pose_image|rknn_pose_stream|sh): .*not found",
]
```

## 验证分层

1. **Host build**
   - 确认 CMake 能编译新增二进制。

2. **Linux board smoke**
   - 在板子 Linux rootfs 里运行固定图片推理。
   - 再运行 UVC 短时流式推理。
   - 成功条件：模型 init 成功、至少完成 1 次 pose inference、程序正常退出。

3. **StarryOS board smoke**
   - 使用 `cargo xtask starry app board` 跑短时配置。
   - 成功条件：打印 `UVC_POSE_STREAM_DONE`。

4. **人工观察**
   - 打开 `http://<board-ip>:8080/stream.mjpg`。
   - 确认骨架点位基本贴合人体。

## 风险和处理

- **模型资产缺失**
  - 当前仓库没有 pose RKNN 模型，需要先准备 detector 和 landmark `.rknn`。

- **输出 tensor layout 不确定**
  - 必须以实际模型的 `rknn_query` tensor 信息为准。
  - 先在 Linux 上打印 input/output attrs，再实现后处理。

- **性能不稳定**
  - 第一版默认 `--infer-every 2` 或更低频率。
  - 先保证摄像头采集不被推理阻塞。

- **ROI 映射错误**
  - 先用固定图片调试。
  - 打印 detector box、ROI、landmark 前几个点的原图坐标。

- **完整 MediaPipe 依赖过重**
  - 第一版只复刻必要的 detector/landmark/postprocess，不引入完整 graph runtime。

## 里程碑

1. 准备并确认 pose detector/landmark RKNN 模型能在 RK3588 Linux 上 init。
2. 实现固定图片 `rknn_pose_image`，输出人体框和 33 点。
3. 实现骨架绘制。
4. 接入 UVC stream，输出 annotated MJPEG。
5. 增加 Linux smoke 命令和 StarryOS board config。
6. 补充短 benchmark，记录 inference FPS 和错误计数。

## 当前推进状态

已先落地固定图片 RKNN probe：

- `cpp/pose_model.h`
- `cpp/pose_model.cc`
- `cpp/pose_image_main.cc`
- CMake target: `rknn_pose_image`

当前 `rknn_pose_image` 已完成：

- 加载 detector/landmark 两个 `.rknn`；
- 打印 input/output tensor attr；
- 对输入图片做整图 letterbox；
- 执行 RKNN inference；
- 可通过 `--dump-output-stats` 打印 raw output byte 统计；
- 成功结束时打印 `POSE_IMAGE_PROBE_DONE`。

当前尚未完成：

- detector 输出解码；
- landmark ROI crop；
- landmark 输出解码；
- 33 点坐标回映射；
- 骨架绘制；
- UVC streaming pose 入口。

下一步需要先拿到实际 pose detector/landmark RKNN 模型，并用
`rknn_pose_image --dump-output-stats` 确认 output tensor layout，然后实现对应
postprocess。

## 模型转换路径

转换脚本使用 OpenCV Zoo 提供的 MediaPipe Pose ONNX 资源，绕过 RKNN-Toolkit2
的 TFLite frontend：

```text
/tmp/mediapipe_pose/onnx/person_detection_mediapipe_2023mar.onnx
/tmp/mediapipe_pose/onnx/pose_estimation_mediapipe_2023mar.onnx
```

已新增转换工具：

```text
tools/convert-mediapipe-pose-rknn.py
tools/convert-mediapipe-pose-rknn-venv.sh
```

已确认该 ONNX 路径可以在 amd64 Docker/qemu 环境中完成 `load_onnx`、
`build` 和 `export_rknn`，并生成：

```text
rknn-mediapipe-pose-image/model/pose_detector.rknn
rknn-mediapipe-pose-image/model/pose_landmark_lite.rknn
```

TFLite 路径不作为提交后的主路径：

- amd64 Docker 容器通过 qemu-user 可以安装并导入 RKNN-Toolkit2 2.3.2。
- 但在 `rknn.load_tflite()` 解析 MediaPipe TFLite 时会触发 qemu 段错误。
- 在真实 x86_64 主机上，`.task` 解包模型和 legacy direct TFLite 资源都观察
  到过 `rknn.load_tflite()` 段错误。
- `tflite2onnx` 绕路失败，原因是该模型包含不支持的 TFLite `DENSIFY` op。
- `tf2onnx --tflite` 也会走 TensorFlow Lite 解析，并在 qemu 下同样段错误。

模型转换仍建议在真实 x86_64 Linux 环境运行；ONNX 路径也已在 amd64
Docker/qemu 环境里完成过 smoke test。转换命令见 README 的 pose model
bring-up 小节，主路径使用 Python 3.10 venv，不依赖 Docker。
