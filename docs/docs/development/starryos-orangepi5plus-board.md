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
- U-Boot 已换成 `schneid-l/u-boot-orangepi5` 系列镜像，`loady` 可用。
- `cargo xtask starry quick-start orangepi-5-plus run` 可以通过串口 YMODEM 传输并启动 StarryOS。
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

已验证的收敛结论：

- `ymodem.rs` 不需要本地修改。
- 去掉 `uboot-shell` timeout 忽略后，U-Boot 命令阶段可到 `loady`，但 YMODEM 尾部可能以 `Operation timed out` 失败。
- 去掉 post-`bootm` reader timeout 忽略后，YMODEM 和 `bootm` 可开始，但内核输出阶段可能以 `failed to read serial output` 失败。
- `tokio_serial::open_native_async()` 在 Mac + OrbStack + `/dev/cu.usbserial-0001` 组合下曾出现接收不可靠；`serialport` 路径与 `picocom` 行为一致，已验证可用。

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

### U-Boot 网口至少一个可用

新版 U-Boot 下 `net list` 能看到 RTL8169：

```text
eth0 : eth_rtl8169 ...
eth1 : eth_rtl8169 ...
```

板子直连 Windows PC，Windows 配 `192.168.10.1/24`，U-Boot 配 `192.168.10.2/24` 后：

- `eth0` ping 失败。
- `eth1` ping 成功。

这说明硬件和 U-Boot 网络驱动至少有一条链路可用。Mac 拓展坞到位后，TFTP 应优先从 `eth1` 验证。

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

这属于 FIT 格式 / U-Boot 校验规则问题。当前自动 run 已通过 `.bin` + YMODEM + `bootm` 链路启动，不要把 FIT 问题误判为 StarryOS 内核启动失败。

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

### 自动 run

```bash
cd /home/wangyifan/lab/tgoskits-board
source /home/wangyifan/lab/env.sh

RUST_LOG=info cargo xtask starry quick-start orangepi-5-plus run \
  --serial /dev/cu.usbserial-0001 \
  --baud 1500000 |& tee logs/starry-orangepi-auto-run.log
```

操作经验：

- 先启动 runner，再复位或重新上电板子。
- 如果停在 `Waiting for board on power or reset...`，先不要退出 runner，先复位板子。
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

Windows 侧曾用 `192.168.10.1/24`。U-Boot 侧：

```text
pci enum
net list
setenv ipaddr 192.168.10.2
setenv serverip 192.168.10.1
setenv netmask 255.255.255.0
setenv gatewayip 192.168.10.1
setenv ethact eth1
ping ${serverip}
```

已知 `eth1` 成功过。

### Mac TFTP 后续方向

这条链路还没有在 Mac 拓展坞上完整验证。它是后续替代 YMODEM 的提速方向，不应被当成当前已完成链路。

macOS 终端侧准备 TFTP 目录：

```bash
sudo mkdir -p /private/tftpboot
sudo chmod 777 /private/tftpboot
sudo cp image.fit /private/tftpboot/image.fit
sudo launchctl load -F /System/Library/LaunchDaemons/tftp.plist
```

U-Boot 侧预期方向：

```text
setenv ipaddr 192.168.10.2
setenv serverip 192.168.10.1
setenv ethact eth1
ping ${serverip}
tftpboot 0x02000000 image.fit
bootm 0x02000000
```

注意：上面的 macOS 命令在 Mac 终端执行，不是在 OrbStack Linux 中执行。

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
3. 在 Mac 有线网口可用后验证 TFTP，把 YMODEM 从主路径降级为 fallback。
4. 单独调查 RK3588 DWMMC IRQ completion，尽量用根因修复替代 polling workaround。

修改逻辑后按项目要求跑相关 clippy。对当前 SD 驱动改动，优先：

```bash
cd /home/wangyifan/lab/tgoskits-board
cargo xtask clippy --package ax-driver
```
