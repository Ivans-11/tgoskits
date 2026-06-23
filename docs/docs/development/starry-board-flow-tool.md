# Starry Board Flow Tool

`tools/starry-board-flow.py` 是一个组合式板端工作流工具，用于把 stock Linux
侧部署、Linux 侧测试、one-shot 启动 StarryOS、StarryOS 侧测试这些步骤拆成可单独运行的小命令。

工具以独立 case 目录为入口，不再从 `apps/starry/<case>` 自动发现流程配置。`apps/starry`
只保留 app 本体、构建脚本和运行资产；板端测试流程放到 `board-cases/<case>/`。

## Case 目录

一个 case 目录至少包含 `board-flow.toml`：

```text
board-cases/<case>/
  board-flow.toml
  board-orangepi-5-plus-smoke.toml
```

`board-flow.toml` 示例：

```toml
[case]
name = "orangepi5plus-smoke"
app_dir = "../../apps/starry/my-app"

[assets]
build = ["./build-assets.sh"]

[[assets.items]]
source = "install/app_payload"
target = "/app_payload"

[linux]
command = '''
cd /app_payload &&
./smoke-test &&
echo APP_LINUX_DONE
'''
success_regex = ["(?m)^APP_LINUX_DONE$"]
fail_regex = ["(?i)\\bpanic(?:ked)?\\b", "(?i)not found"]
timeout = 120

[starry]
board_config = "board-orangepi-5-plus-smoke.toml"
```

路径规则：

- `case.app_dir` 相对 case 目录解析，也可以写绝对路径。
- `assets.build` 在 `case.app_dir` 下执行。
- `assets.items.source` 相对 `case.app_dir` 解析。
- `starry.board_config` 相对 case 目录解析；不写时，如果 case 目录只有一个 `board-*.toml`，工具会自动使用它。

## 基础参数

常用连接参数：

```bash
--board-ip 192.168.10.2
--ssh-user orangepi
--ssh-password <password>
--serial /dev/tty.usbserial-0001
--baud 1500000
```

`BOARD_IP`、`BOARD_USER`、`BOARD_PASSWORD` 环境变量也可以覆盖默认值。工具使用 `sshpass -e`
支持非交互 SSH/SCP；如果主机缺少该命令，需要先安装。没有设置密码时，工具会尝试走 SSH key。
后续示例省略密码参数，默认已经通过 `BOARD_PASSWORD` 或 `--ssh-password` 提供密码。

## 单独上传任意路径

`upload-path` 不依赖 case 目录。

上传文件：

```bash
tools/starry-board-flow.py upload-path ./local-file /remote/path/file \
  --board-ip 192.168.10.2
```

上传目录：

```bash
tools/starry-board-flow.py upload-path ./local-dir /remote/dir \
  --board-ip 192.168.10.2
```

目录上传会先打成 tar 包传到 `/tmp`，再用 sudo 解包到目标目录，并执行 `sync`。

## 构建和上传资产

只构建资产：

```bash
tools/starry-board-flow.py build-assets board-cases/<case>
```

构建并上传：

```bash
tools/starry-board-flow.py upload-assets board-cases/<case> --build \
  --board-ip 192.168.10.2
```

只上传已经构建好的资产：

```bash
tools/starry-board-flow.py upload-assets board-cases/<case> \
  --board-ip 192.168.10.2
```

## 上传 FIT

`upload-fit` 不依赖 case 目录。默认 FIT 路径是 `tmp/axbuild/image.fit`：

```bash
tools/starry-board-flow.py upload-fit \
  --board-ip 192.168.10.2
```

显式指定 FIT：

```bash
tools/starry-board-flow.py upload-fit \
  --fit tmp/axbuild/image.fit \
  --board-ip 192.168.10.2
```

FIT 会安装为板端 Linux 的 `/boot/starry/image.fit`。

## 只跑 Linux 测试

使用 `board-flow.toml` 的 `[linux].command`：

```bash
tools/starry-board-flow.py linux-test board-cases/<case> \
  --board-ip 192.168.10.2
```

临时覆盖命令：

```bash
tools/starry-board-flow.py linux-test board-cases/<case> \
  --command 'cd /app_payload && ./smoke-test && echo DONE' \
  --board-ip 192.168.10.2
```

## 只跑 StarryOS 测试

默认会通过 `/boot/starry-boot.env` 写入 one-shot 标记，然后 `reboot` 进 StarryOS：

```bash
tools/starry-board-flow.py starry-test board-cases/<case> \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

指定 board config：

```bash
tools/starry-board-flow.py starry-test board-cases/<case> \
  --board-config board-orangepi-5-plus-smoke.toml \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

如果板子已经在 StarryOS shell，可以跳过 one-shot reboot，只在串口上等待 prompt 并执行命令：

```bash
tools/starry-board-flow.py starry-test board-cases/<case> \
  --no-reboot \
  --serial /dev/tty.usbserial-0001
```

## 串口临时命令

`serial-run` 不依赖 case 目录，适合临时调试：

```bash
tools/starry-board-flow.py serial-run \
  --serial /dev/tty.usbserial-0001 \
  --wait-for 'root@starry:/root #' \
  --command 'pwd; ls -l /; echo SERIAL_DONE' \
  --success-regex '(?m)^SERIAL_DONE$'
```

如果串口打开时系统已经停在 shell prompt 且没有新输出，工具默认会在 1 秒静默后
发送一次回车来触发 prompt。需要纯被动监听时可加 `--no-prompt-nudge`。

## 完整组合流程

构建并上传资产、上传 FIT、先跑 Linux、再跑 StarryOS：

```bash
tools/starry-board-flow.py run board-cases/<case> \
  --deploy-assets --build-assets \
  --deploy-fit --fit tmp/axbuild/image.fit \
  --side both \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

不重传资产和 FIT，只重跑两侧测试：

```bash
tools/starry-board-flow.py run board-cases/<case> \
  --side both \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

只跑 Linux：

```bash
tools/starry-board-flow.py run board-cases/<case> \
  --side linux \
  --board-ip 192.168.10.2
```

只跑 StarryOS：

```bash
tools/starry-board-flow.py run board-cases/<case> \
  --side starry \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

## 回到 Linux

当前 Orange Pi 5 Plus stock U-Boot 流程使用 one-shot 标记：

1. Linux 写 `/boot/starry-boot.env = starry_boot=1`。
2. U-Boot 读到 `starry_boot=1` 后，先用 `fatwrite` 把标记改回 `starry_boot=0`。
3. U-Boot 再启动 `/boot/starry/image.fit`。
4. 下一次复位默认回 stock Linux。

因此 StarryOS 侧测试结束后，不要求 StarryOS 写回标记。当前板子没有接入自动电源控制，且 StarryOS 软件 reboot/reset 路径不可用，所以需要手动断电/上电。

如果要让完整流程等待 Linux 回来：

```bash
tools/starry-board-flow.py run board-cases/<case> \
  --side starry \
  --wait-linux \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

如果后续接入可脚本化电源控制，可加：

```bash
tools/starry-board-flow.py run board-cases/<case> \
  --side starry \
  --wait-linux \
  --power-cycle-command './power-cycle-board.sh' \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

## RKNN YOLO 和 MediaPipe Pose 示例

当前 YOLO 和 MediaPipe pose 是两个独立 case，但都通过 `case.app_dir` 引用
`apps/starry/orangepi-5-plus-uvc-rknn` 中的构建脚本和运行资产：

- `board-cases/orangepi5plus-rknn-yolo/`：YOLOv8 固定图片 smoke test。
- `board-cases/orangepi5plus-mediapipe-pose/`：MediaPipe pose 固定图片 probe。

两个 case 的构建目标是分开的：YOLO case 只执行 `./build-image-runner.sh yolo`，
pose case 只执行 `./build-image-runner.sh pose`。pose 的两个 RKNN 模型按 YOLO
模型的方式随仓库提交；只有需要更新模型时，才在 x86_64 主机重新生成
`rknn-mediapipe-pose-image/model/*.rknn` 并提交刷新后的文件。

YOLO 完整命令示例：

```bash
tools/starry-board-flow.py run board-cases/orangepi5plus-rknn-yolo \
  --deploy-assets --build-assets \
  --deploy-fit --fit tmp/axbuild/image.fit \
  --side both \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

MediaPipe pose 完整命令示例：

```bash
tools/starry-board-flow.py run board-cases/orangepi5plus-mediapipe-pose \
  --deploy-assets --build-assets \
  --deploy-fit --fit tmp/axbuild/image.fit \
  --side both \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

只重跑测试时更换 case 目录即可：

```bash
tools/starry-board-flow.py run board-cases/orangepi5plus-rknn-yolo \
  --side both \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2

tools/starry-board-flow.py run board-cases/orangepi5plus-mediapipe-pose \
  --side both \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```
