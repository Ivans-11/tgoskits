---
sidebar_position: 30
sidebar_label: "Orange Pi 5 Plus 板端 StarryOS"
---

# Orange Pi 5 Plus 板端 StarryOS 开发交接

这份文档不是测试手册。它的目标是让接手者快速知道当前已经做到哪一步、哪些问题已经排过、哪些本地补丁在生效，以及真正需要复现时该参考哪些命令。不要把下面的命令当成接手后的必跑清单。

## 当前结论

当前本地 Orange Pi 5 Plus / RK3588 板端 StarryOS 自动运行链路已经跑通过。

已确认的工作链路：

- 主机是 MacBook，构建和运行命令在 OrbStack Linux 中执行。
- 串口在 OrbStack Linux 中可见为 `/dev/cu.usbserial-0001`。
- 串口波特率是 `1500000`。
- U-Boot 已换成 `schneid-l/u-boot-orangepi5` 系列镜像，`loady` 和 TFTP 均可用。
- 当前主开发链路是 Mac 有线网口 TFTP：Mac `192.168.10.1/24`，板端 U-Boot `192.168.10.2/24`，使用 `eth1`。
- `cargo xtask starry quick-start orangepi-5-plus run` 已可通过 TFTP 传输 FIT image 并启动 StarryOS。
- 串口 YMODEM / `loady` 仍可作为 fallback，但不再是主开发路径。
- 自动 run 最终已到达 `root@starry:/root #`。
- 当前成功依赖本地 `ostool` / `uboot-shell` 补丁。
- 当前 SD 卡 ext4 写入稳定性依赖 `rockchip_sd.rs` 中的 polling workaround。

当前开发分支：

```bash
cd /home/wangyifan/lab/tgoskits-board
git status --short --branch
```

创建本文档时的状态：

```text
## board/starry-orangepi-run
 M drivers/ax-driver/src/block/rockchip_sd.rs
?? docs/docs/development/starryos-orangepi5plus-board.md
```

## 关键本地改动

### tgoskits-board

当前 tracked 逻辑改动只有：

```text
drivers/ax-driver/src/block/rockchip_sd.rs
```

它把 RK3588 DWMMC block device 的完成方式临时改为 polling：

```rust
SdmmcBlockConfig::dma("rockchip-sd", card_info.capacity_blocks.unwrap_or(0), false)
```

背景：

- StarryOS 可以挂载 SD 卡 ext4 rootfs。
- IRQ-driven completion 在文件系统写入 / sync 路径上曾卡住。
- polling workaround 能让当前板端 StarryOS 路径继续开发。
- 这不是 RK3588 DWMMC IRQ 根因修复。后续如果恢复 IRQ completion，需要单独验证 ext4 mount、写入、sync、Linux 回读和 fsck。

### 本地 ostool

本地补丁仓库：

```text
/home/wangyifan/lab/ostool
```

分支：

```text
tgoskits-board-serial-timeout-v0.23.4
```

修改文件：

```text
ostool/src/run/uboot.rs
uboot-shell/src/lib.rs
```

补丁内容：

- `ostool` 本地串口 backend 改用 `serialport::new(...).timeout(200ms).flow_control(FlowControl::None).open()`，再用 `try_clone()` 分出读写端，并通过 `futures::io::AllowStdIo` 接入现有 trait。
- `uboot-shell::read_byte_with_timeout` 忽略底层 `TimedOut` / `WouldBlock`，直到外层命令超时真正到期。
- `ostool` 在 `bootm` 后的终端 reader 中也忽略 `TimedOut` / `WouldBlock`，避免 StarryOS 已启动但串口普通超时被当成 fatal error。
- `ostool` 在准备 U-Boot runtime config 时，如果 `local.net` 为空，会把顶层 `[net]` fallback 到 `local.net`。这是当前本地串口 TFTP 路径能从 TOML `[net]` 配置生效的前提。

已验证的收敛结论：

- `ymodem.rs` 不需要本地修改。
- 去掉 `uboot-shell` timeout 忽略后，U-Boot 命令阶段可到 `loady`，但 YMODEM 尾部可能以 `Operation timed out` 失败。
- 去掉 post-`bootm` reader timeout 忽略后，YMODEM 和 `bootm` 可开始，但内核输出阶段可能以 `failed to read serial output` 失败。
- `tokio_serial::open_native_async()` 在 Mac + OrbStack + `/dev/cu.usbserial-0001` 组合下曾出现接收不可靠；`serialport` 路径与 `picocom` 行为一致，已验证可用。
- 原始 `ostool` 结构里顶层 `UbootConfig.net` 和 flattened `LocalUbootConfig.net` 重名，TOML `[net]` 会落到顶层 `net`，而 local runner 实际只看 `local.net`。没有上述 fallback 时，即使 TOML 中写了 `[net]`，本地串口 runner 仍会打印 `No TFTP config, using loady to upload FIT image...`。

### Cargo override

`tgoskits-board/.cargo/.config-local.toml` 指向本地 `ostool`：

```toml
paths = [
    "../ostool/ostool",
    "../ostool/uboot-shell",
]
```

这只是本地开发覆盖，不是最终上游方案。Cargo 会提示 path override 改变依赖图；当前可接受，但长期更适合把补丁发到 `ostool` 上游，或改成更明确的 `[patch.crates-io]` 方案。

拉取远程更新后，需要确认：

- `Cargo.lock` 中的 `ostool` / `uboot-shell` 版本是否仍和本地补丁分支匹配。
- `cargo tree -i ostool` 和 `cargo tree -i uboot-shell` 是否仍解析到本地路径。
- 本地补丁是否还能无冲突套在新版本上。

## 已排查过的问题

### 旧 Armbian U-Boot 不适合当前自动链路

旧 U-Boot 不支持 `loady`：

```text
Unknown command 'loady'
```

换成 `schneid-l/u-boot-orangepi5` 系列镜像后，已确认：

```text
U-Boot 2026.04
loady - load binary file over serial line (ymodem mode)
```

这就是当前自动 YMODEM 链路能工作的前提。

### 串口链路本身是好的

`picocom -b 1500000 /dev/cu.usbserial-0001` 能看到 U-Boot 和 Linux/StarryOS 串口输出。之前自动 runner 收不到输出的问题不是线序或波特率问题，而是本地 `ostool` 串口 backend / timeout 处理问题。

### U-Boot 网口和 TFTP 已可用

新版 U-Boot 下 `net list` 能看到 RTL8169：

```text
eth0 : eth_rtl8169 ...
eth1 : eth_rtl8169 ...
```

早期验证中，板子直连 Windows PC，Windows 配 `192.168.10.1/24`，U-Boot 配 `192.168.10.2/24` 后：

- `eth0` ping 失败。
- `eth1` ping 成功。

当前主链路已经切到 Mac 有线网口 TFTP：

- Mac 有线网口静态配置为 `192.168.10.1/24`。
- U-Boot 使用 `eth1`，`ipaddr=192.168.10.2`，`serverip=192.168.10.1`。
- macOS 系统 TFTP 服务已通过 `launchctl load -F /System/Library/LaunchDaemons/tftp.plist` 启用。
- OrbStack Linux 中可通过 `/mnt/mac/private/tftpboot` 写入 Mac 的 `/private/tftpboot`。
- 手动 `tftpboot 0x02000000 ping.txt` 已成功，`ping.txt` 传输 `3` 字节。
- 自动 run 已通过 TFTP 拉取约 `13.1 MiB` 的 `image.fit`，日志中出现 `Bytes transferred = 14010672`，传输速率约 `3 MiB/s`，随后成功进入 `root@starry:/root #`。

注意：自动链路里必须在 TFTP 前显式执行 `pci enum` 和 `net list`。没有这两个 U-Boot 命令时，后续 `tftp` 可能报 `No ethernet found.`。

### FIT 路径不要和当前自动 run 混为一谈

曾在 U-Boot 中手动：

```text
load mmc 1:1 0x02000000 /boot/image.fit
bootm 0x02000000
```

遇到：

```text
Bad FIT kernel image format! (err=-99)
Signature checking prevents use of unit addresses (@) in nodes
```

这属于 FIT 格式 / U-Boot 校验规则问题。当前自动 run 使用 `ostool` 生成的 FIT image，并通过 TFTP + `bootm` 链路启动成功；不要把历史手写 FIT 问题误判为 StarryOS 内核启动失败。

### SD 卡 rootfs 已可用，但设备名必须谨慎

SD 卡 ext4 rootfs 已用于板端启动和挂载验证。此前 OrbStack Linux 与 macOS 对读卡器可见性不完全一致；如果 Linux 看不到 Mac 磁盘工具里能看到的 SD 卡，需要处理 USB 直通、换读卡器，或在 macOS 侧先处理分区。

格式化时必须确认设备名。之前目标分区出现过类似提示：

```text
/dev/sda1 contains a ext4 file system labelled 'armbi_root'
last mounted on / ...
```

只有确认它确实是目标 SD 卡分区后才能继续。

## 需要复现时的命令参考

这些命令用于复现或排障，不是接手后的必跑流程。

### 自动 run（当前主路径：TFTP）

Mac 侧一次性准备：

```bash
sudo mkdir -p /private/tftpboot
sudo chmod 777 /private/tftpboot
sudo launchctl load -F /System/Library/LaunchDaemons/tftp.plist
```

`launchctl load` 后通常不需要每次开发前重复执行；只要没有手动 `unload` 或重启后状态丢失，TFTP 服务会继续可用。需要检查时：

```bash
sudo launchctl print system/com.apple.tftpd
```

需要关闭时：

```bash
sudo launchctl unload -F /System/Library/LaunchDaemons/tftp.plist
```

Mac 有线网口配置：

```text
IP address: 192.168.10.1
Subnet mask: 255.255.255.0
Router: empty
DNS: empty
```

OrbStack Linux 中确认 Mac TFTP 目录可见：

```bash
ls -l /mnt/mac/private/tftpboot
```

当前本地 U-Boot runtime 配置在：

```text
tmp/axbuild/config/starryos/quick-start/orangepi-5-plus-uboot.toml
```

应包含：

```toml
serial = "/dev/cu.usbserial-0001"
baud_rate = "1500000"
dtb_file = "/home/wangyifan/lab/tgoskits-board/os/StarryOS/configs/board/orangepi-5-plus.dtb"

uboot_cmd = [
    "pci enum",
    "net list",
    "setenv ethact eth1",
    "setenv ipaddr 192.168.10.2",
    "setenv serverip 192.168.10.1",
    "setenv netmask 255.255.255.0",
    "setenv gatewayip 192.168.10.1",
]

success_regex = []
fail_regex = ["panicked at"]

[net]
interface = "mac-tftp"
board_ip = "192.168.10.2"
netmask = "255.255.255.0"
gatewayip = "192.168.10.1"
tftp_dir = "/mnt/mac/private/tftpboot"
```

`interface = "mac-tftp"` 是有意使用的 dummy 名称。构建和 runner 在 OrbStack Linux 中执行，真正 TFTP server 在 Mac 侧；不要让 `ostool` 根据 OrbStack 内部网卡自动推导 `serverip`，而是通过 `uboot_cmd` 固定 `serverip=192.168.10.1`。

```bash
cd /home/wangyifan/lab/tgoskits-board
source /home/wangyifan/lab/env.sh

RUST_LOG=info cargo xtask starry quick-start orangepi-5-plus run \
  --serial /dev/cu.usbserial-0001 \
  --baud 1500000 |& tee logs/starry-orangepi-tftp-run.log
```

操作经验：

- 先启动 runner，再复位或重新上电板子。
- 如果停在 `Waiting for board on power or reset...`，先不要退出 runner，先复位板子。
- 正常情况下会先看到 `Linux detected: using net.tftp_dir=/mnt/mac/private/tftpboot`，然后 U-Boot 执行 `tftp ... image.fit && bootm`。
- 成功标志是 `root@starry:/root #`。

串口占用检查：

```bash
ps -ef | rg 'tg-xtask|cargo xtask|picocom|cu.usbserial' || true
```

如果确认旧进程占用串口：

```bash
kill -INT <pid>
```

### picocom 串口验证

```bash
picocom -b 1500000 /dev/cu.usbserial-0001
```

U-Boot 提示符下常用检查：

```text
version
help loady
pci enum
mmc list
net list
```

退出 `picocom`：

```text
Ctrl-A
Ctrl-X
```

### 本地 ostool 检查

```bash
cd /home/wangyifan/lab/ostool
git status --short --branch
git diff -- ostool/src/run/uboot.rs uboot-shell/src/lib.rs
cargo fmt
cargo check -p ostool -p uboot-shell

cd /home/wangyifan/lab/tgoskits-board
cat .cargo/.config-local.toml
cargo tree -i ostool
cargo tree -i uboot-shell
```

历史上完整跑通过：

```bash
cd /home/wangyifan/lab/ostool
cargo test -p uboot-shell
cargo test -p ostool
cargo clippy -p uboot-shell -p ostool --all-targets
```

### U-Boot 网口验证

当前 Mac 侧用 `192.168.10.1/24`。U-Boot 侧：

```text
pci enum
net list
setenv ethact eth1
setenv ipaddr 192.168.10.2
setenv serverip 192.168.10.1
setenv netmask 255.255.255.0
setenv gatewayip 192.168.10.1
ping ${serverip}
```

已知 `eth1` 成功。若 ping 只在重新 `setenv` 后第一次成功，不必先追根；当前自动链路会在每次启动前重新写入这些变量。

TFTP smoke：

macOS 终端侧准备 TFTP 目录：

```bash
sudo mkdir -p /private/tftpboot
sudo chmod 777 /private/tftpboot
echo ok | sudo tee /private/tftpboot/ping.txt
sudo launchctl load -F /System/Library/LaunchDaemons/tftp.plist
```

U-Boot 侧：

```text
setenv ethact eth1
setenv ipaddr 192.168.10.2
setenv serverip 192.168.10.1
setenv netmask 255.255.255.0
setenv gatewayip 192.168.10.1
ping ${serverip}
tftpboot 0x02000000 ping.txt
```

正常 TFTP smoke 输出中应看到：

```text
Bytes transferred = 3 (3 hex)
```

注意：上面的 macOS 命令在 Mac 终端执行，不是在 OrbStack Linux 中执行。OrbStack 侧通过 `/mnt/mac/private/tftpboot` 写入同一个 TFTP 根目录。

### SD 卡 rootfs 参考

确认设备名：

```bash
lsblk -o NAME,SIZE,TYPE,FSTYPE,LABEL,MOUNTPOINTS
dmesg | tail -n 80
```

同步 rootfs 的参考流程：

```bash
export SD_PART=/dev/sdX1
export ROOTFS_SRC=/mnt/starry-src
export ROOTFS_DST=/mnt/starry-sd

sudo apt update
sudo apt install -y rsync

sudo mkfs.ext4 -L rootfs "$SD_PART"
sudo mkdir -p "$ROOTFS_DST"
sudo mount "$SD_PART" "$ROOTFS_DST"
sudo rsync -aHAX --numeric-ids "$ROOTFS_SRC"/ "$ROOTFS_DST"/
sync
sudo umount "$ROOTFS_DST"
```

`/dev/sdX1` 必须替换成实际确认的目标 SD 卡分区。

### U-Boot SD 卡检查

```text
mmc list
mmc dev 1
mmc rescan
mmc info
part list mmc 1
fstype mmc 1:1
ls mmc 1:1 /
ls mmc 1:1 /boot
ext4ls mmc 1:1 /
ext4ls mmc 1:1 /boot
```

正常情况下能看到：

```text
mmc@fe2c0000: 1 (SD)
mmc@fe2e0000: 0
Partition Type: EFI
1 ... "rootfs"
```

如果出现 `Can't set block device`，先重新插拔 SD 卡或回 Linux/macOS 侧检查文件系统，不要直接归因到 StarryOS。

## 后续开发建议

优先级建议：

1. 把 `ostool` / `uboot-shell` 本地补丁整理成上游 PR 或可维护的 patch 方案。
2. 决定 `tgoskits-board` 侧是否继续使用 `.cargo/.config-local.toml`，还是切换到更明确的 `[patch.crates-io]`。
3. 把当前 TFTP 主路径固化到更可维护的配置方案中，避免长期依赖 `tmp/axbuild/.../orangepi-5-plus-uboot.toml` 手工状态。
4. 将 `ostool` 顶层 `[net]` fallback 到 local runner 的补丁整理成上游 PR。
5. 单独调查 RK3588 DWMMC IRQ completion，尽量用根因修复替代 polling workaround。

修改逻辑后按项目要求跑相关 clippy。对当前 SD 驱动改动，优先：

```bash
cd /home/wangyifan/lab/tgoskits-board
cargo xtask clippy --package ax-driver
```
