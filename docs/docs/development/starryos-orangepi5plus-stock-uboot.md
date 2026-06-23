---
sidebar_position: 31
sidebar_label: "Orange Pi 5 Plus Stock U-Boot 启动 StarryOS"
---

# Orange Pi 5 Plus 基于 Stock U-Boot 手工启动 StarryOS

这份文档记录一条已经实际跑通的备用路径：

- 板子使用官方 Ubuntu SD 镜像启动 Linux。
- SPI 已擦除，不再依赖 `schneid-l/u-boot-orangepi5`。
- 进入 SD 卡自带的 `U-Boot 2017.09-orangepi`。
- 主机侧生成 `image.fit`。
- 通过 `scp` 把 `image.fit` 传到板子 Linux。
- 回到 U-Boot，手工 `load` + `bootm` 启动 StarryOS。

这不是当前仓库中已有的主自动化链路。当前主自动化链路仍是旧文档中记录的 `schneid-l` U-Boot + TFTP/FIT 路径。本文只记录当前这次在 stock U-Boot 下跑通的经验。

## 适用前提

- 开发板：Orange Pi 5 Plus / RK3588
- Linux 镜像：`Orangepi5plus_1.2.0_ubuntu_jammy_server_linux6.1.43.img`
- SPI 状态：已擦除，板子上电后默认直接进 Linux
- U-Boot：`U-Boot 2017.09-orangepi`

当前观察到的关键现象：

- 这份 stock U-Boot 的命令能力和之前 `schneid-l` 那份不一样。
- `loady` 不可用，旧文档中的串口 YMODEM 路径不能直接复用。
- U-Boot 网络/TFTP 能力也不应假定可直接复用。
- 但从 Linux 侧 `scp` 文件到 SD 卡，再让 U-Boot 从 SD 卡读 `image.fit`，是可行的。

## 当前结论

这条路径已经确认可用：

1. 主机构建 `starryos.bin`
2. 主机本地打包 `image.fit`
3. `scp` 到板子 Linux
4. 把 `image.fit` 放到 Linux 的 `/boot/starry/image.fit`
5. 在 U-Boot 中从 `mmc 0:1` 读取 `/starry/image.fit`
6. 用显式组件形式执行 `bootm`

最终跑通的 U-Boot 命令是：

```text
mmc dev 0
mmc rescan
load mmc 0:1 0x05480000 /starry/image.fit
bootm 0x05480000:kernel - 0x05480000:fdt
```

注意：这里验证通过的是显式子镜像形式，不是裸 `bootm 0x05480000`。

## 当前板上地址

当前板上通过 `printenv` 看到的关键地址是：

```text
kernel_addr_r=0x00400000
fdt_addr_r=0x0a100000
kernel_addr_c=0x05480000
```

本文中生成 FIT 和 U-Boot 加载命令都基于这些地址。

## 主机侧构建 StarryOS

在仓库根目录执行：

```bash
cargo xtask starry quick-start orangepi-5-plus build
```

当前实际产物路径是：

- `target/aarch64-unknown-linux-musl/release/starryos`
- `target/aarch64-unknown-linux-musl/release/starryos.bin`

其中 U-Boot/FIT 路径实际用的是 `starryos.bin`。

## 主机侧生成 image.fit

### 1. 准备 ITS 文件

本文验证通过的 ITS 内容如下：

```its
/dts-v1/;

/ {
    description = "StarryOS Orange Pi 5 Plus";
    #address-cells = <1>;

    images {
        kernel {
            description = "StarryOS kernel";
            data = /incbin/("target/aarch64-unknown-linux-musl/release/starryos.bin");
            type = "kernel";
            arch = "arm64";
            os = "linux";
            compression = "none";
            load = <0x00400000>;
            entry = <0x00400000>;
            hash-1 {
                algo = "sha256";
            };
        };

        fdt {
            description = "rk3588-orangepi-5-plus";
            data = /incbin/("os/StarryOS/configs/board/orangepi-5-plus.dtb");
            type = "flat_dt";
            arch = "arm64";
            compression = "none";
            load = <0x0a100000>;
            hash-1 {
                algo = "sha256";
            };
        };
    };

    configurations {
        default = "conf-1";
        conf-1 {
            description = "StarryOS Orange Pi 5 Plus";
            kernel = "kernel";
            fdt = "fdt";
        };
    };
};
```

可以把它保存成例如：

```text
tmp/axbuild/orangepi-5-plus-image.its
```

### 2. 生成 FIT

```bash
mkimage -f tmp/axbuild/orangepi-5-plus-image.its tmp/axbuild/image.fit
```

### 3. 校验 FIT

```bash
mkimage -l tmp/axbuild/image.fit
```

当前实际生成成功的镜像信息：

- kernel load/entry：`0x00400000`
- fdt load：`0x0a100000`
- `image.fit` 大小约 `14 MiB`

## 板子 Linux 侧联网

如果板子 Linux 启动后没有自动拿到 IP，需要先在 Linux 中把网卡配起来。

本文会话中的一个示例命令是：

```bash
sudo ip addr add 192.168.10.2/24 dev enP4p65s0
```

是否需要每次启动都重新配置，取决于你是否把它写进网络配置。本文只记录临时验证路径，不展开固定网络配置方案。

确认网络后，可用：

```bash
ip addr
```

## 用 scp 把 image.fit 传到板子

主机侧：

```bash
scp tmp/axbuild/image.fit orangepi@<board-ip>:/tmp/image.fit
```

然后登录板子 Linux：

```bash
ssh orangepi@<board-ip>
```

在板子 Linux 里把 FIT 放到启动分区对应位置：

```bash
sudo mkdir -p /boot/starry
sudo mv /tmp/image.fit /boot/starry/image.fit
sync
ls -lh /boot/starry/image.fit
```

这里要特别注意路径映射：

- Linux 中放的是 `/boot/starry/image.fit`
- U-Boot 中读的是 `mmc 0:1` 分区里的 `/starry/image.fit`

换句话说，U-Boot 看到的 `mmc 0:1`，对应的是 Linux 里的 `/boot` 挂载点本身，而不是整个 Linux 根目录。

## 进入 U-Boot

可以在串口里打断自动启动进入 U-Boot。

建议先确认版本和关键环境变量：

```text
version
printenv kernel_addr_r fdt_addr_r kernel_addr_c
```

本文验证时输出为：

```text
kernel_addr_r=0x00400000
fdt_addr_r=0x0a100000
kernel_addr_c=0x05480000
```

## 在 U-Boot 中手工启动 StarryOS

完整命令如下：

```text
mmc dev 0
mmc rescan
load mmc 0:1 0x05480000 /starry/image.fit
bootm 0x05480000:kernel - 0x05480000:fdt
```

这是当前实际跑通的命令组合。

## 用 one-shot wrapper 在 Linux 和 StarryOS 间切换

如果希望板子默认启动 Linux，只在需要时从 Linux 重启进 StarryOS，可以把 `/boot/boot.scr` 换成一个轻量 wrapper。

当前跑通的方案是把 one-shot 标记放在 bootfs，也就是 Linux 里的 `/boot/starry-boot.env`。Linux 只负责把下一次启动请求写成 `starry_boot=1`；U-Boot 读到请求后先把 bootfs 上的标记写回 `starry_boot=0`，然后再启动 StarryOS。这样即使 StarryOS 启动后崩溃或卡住，下一次手动断电/上电也会回到 stock Linux。

注意当前验证过的 U-Boot 行为：

- 不要用 `test -e mmc 0:2 /starry-boot-once`，这个版本会误判为存在。
- 不要用 `ext4ls mmc 0:2 /starry-boot-once` 判断普通文件，文件存在时也可能不能作为可靠判断。
- 不要用“创建/删除 `/starry-boot-once`”作为开关。StarryOS 删除后，Linux 能看到文件不存在，但 U-Boot 仍可能在 ext4 原始视角下读到旧目录项或旧内容。
- 不要依赖 StarryOS 启动后写回 `starry_boot=0`。如果 StarryOS 启动崩溃或卡住，就没有机会执行写回。
- 不要在 U-Boot `fatwrite` 的 bootfs 根目录路径前加 `/`。实测 `fatwrite mmc 0:1 ... /starry-boot.env ...` 会制造或访问异常的 `/starry-boot.env` 条目，导致 `14 bytes written` 之后重新读回仍可能是旧值。这里使用 `starry-boot.env`、`starry-boot-0.env`、`starry/image.fit`、`boot.scr.linux` 这种无前导 `/` 的路径。
- 这个 stock U-Boot 的交互提示符是 `opi#`，中断 autoboot 的热键是 Ctrl-C。

wrapper 源文件如下：

```text
# Orange Pi stock U-Boot wrapper.
# Default: boot stock Linux.
# If bootfs starry-boot.env sets starry_boot=1, reset it to 0 first,
# then boot StarryOS. This keeps Starry boot one-shot even if StarryOS hangs.
echo "STARRY_ONESHOT_WRAPPER_BEGIN"

setenv linux_script_addr "0x09000000"
setenv starry_fit_addr "0x05480000"
setenv starry_env_addr "0x08300000"
setenv starry_boot "0"

if load mmc 0:1 ${starry_env_addr} starry-boot.env; then
    env import -t ${starry_env_addr} ${filesize}
else
    echo "No Starry boot env found on mmc 0:1; defaulting to Linux"
fi

if test "${starry_boot}" = "1"; then
    echo "starry_boot=1; resetting one-shot env before booting StarryOS"
    if load mmc 0:1 ${starry_env_addr} starry-boot-0.env; then
        if fatwrite mmc 0:1 ${starry_env_addr} starry-boot.env ${filesize}; then
            echo "Starry one-shot env reset to starry_boot=0"
            if load mmc 0:1 ${starry_fit_addr} starry/image.fit; then
                bootm ${starry_fit_addr}:kernel - ${starry_fit_addr}:fdt
            fi
        else
            echo "ERROR: failed to reset Starry one-shot env; falling back to Linux"
        fi
    else
        echo "ERROR: failed to load starry-boot-0.env; falling back to Linux"
    fi
    echo "ERROR: failed to boot StarryOS FIT; falling back to Linux"
fi

echo "Booting stock Linux"
if load mmc 0:1 ${linux_script_addr} boot.scr.linux; then
    source ${linux_script_addr}
fi

echo "ERROR: failed to load boot.scr.linux"
```

生成脚本镜像：

```bash
mkimage -A arm64 -T script -C none \
  -n 'OrangePi Starry one-shot wrapper' \
  -d tmp/orangepi-boot-starry-marker.cmd \
  tmp/orangepi-boot-starry-marker.scr
```

安装前先保留 stock Linux 脚本：

```bash
ssh orangepi@<board-ip>
sudo cp /boot/boot.scr /boot/boot.scr.linux
sync
```

准备默认 Linux 状态：

```bash
printf 'starry_boot=0\n' | sudo tee /boot/starry-boot-0.env
printf 'starry_boot=1\n' | sudo tee /boot/starry-boot-1.env
sudo cp /boot/starry-boot-0.env /boot/starry-boot.env
sudo rm -f /starry-boot-once /starry-boot.env
sync
```

安装 wrapper：

```bash
scp tmp/orangepi-boot-starry-marker.scr orangepi@<board-ip>:/tmp/boot.scr
ssh orangepi@<board-ip>
sudo cp /tmp/boot.scr /boot/boot.scr
sync
sha256sum /boot/boot.scr /tmp/boot.scr
```

从 Linux 请求下一次启动 StarryOS：

```bash
sudo cp /boot/starry-boot-1.env /boot/starry-boot.env
sync
sudo reboot
```

StarryOS 已支持通过 `reboot -f` 触发 PSCI `SYSTEM_RESET`。因为 U-Boot 在启动
StarryOS 前已经把 bootfs 标记写回 `starry_boot=0`，所以 StarryOS 测试完成后
软件复位或手动断电/上电都会默认回 stock Linux。下一次 U-Boot 会输出类似：

```text
STARRY_ONESHOT_WRAPPER_BEGIN
reading starry-boot.env
14 bytes read ...
Booting stock Linux
```

`starry_boot=1` 时，已经验证会输出：

```text
STARRY_ONESHOT_WRAPPER_BEGIN
reading starry-boot.env
14 bytes read ...
starry_boot=1; resetting one-shot env before booting StarryOS
reading starry-boot-0.env
14 bytes read ...
writing starry-boot.env
14 bytes written
Starry one-shot env reset to starry_boot=0
reading starry/image.fit
```

已经验证的闭环：

- Linux 写 `starry_boot=0` 后 `sudo reboot`，U-Boot 回 stock Linux。
- Linux 写 `starry_boot=1` 后 `sudo reboot`，U-Boot 先 `fatwrite` 清回 `starry_boot=0`，再启动 StarryOS。
- StarryOS 不写回任何标记，`reboot -f` 软件复位或手动断电/上电后，U-Boot 读取到 `starry_boot=0` 并回 stock Linux。
- 最终 Linux 侧确认 `/boot/starry-boot.env` 内容为 `starry_boot=0`，修正后的 `/boot/boot.scr` 哈希为 `0d73b81a93160fb4e581efce2c52f940addb6d27960594129516aab23ea7c2c1`。

## 组合式自动化流程

`tools/starry-board-flow.py` 把 stock Linux + one-shot Starry 启动流程拆成可单独调用的基础工具：

- `upload-path`：上传任意本地文件或目录到板端 Linux，用于临时传任何文件。
- `upload-assets`：读取 `board-cases/<case>/board-flow.toml`，可先构建 app 资产，再打包上传到板端 Linux。
- `build-fit`：用当前 StarryOS kernel bin 和 DTB 生成 `tmp/axbuild/image.fit`，也可先触发 StarryOS 构建。
- `upload-fit`：把指定 FIT 上传为 `/boot/starry/image.fit`。
- `linux-test`：通过 SSH 在 stock Linux 侧执行 app 声明的测试命令。
- `starry-test`：通过 one-shot 标记重启进 StarryOS，用串口等待 shell，然后执行现有 `board-*.toml` 的 `shell_init_cmd`。
- `serial-run`：直接在串口 shell 上等待指定 prompt 并执行命令，便于临时调试。
- `run`：把上述步骤组合起来，可通过参数选择是否重传资产、是否重传 FIT、只跑 Linux、只跑 Starry，或两边都跑。

`apps/starry` 保留 app 本体、构建脚本和运行资产。板端自动化流程单独放在
`board-cases/<case>/`，case 通过 `[case].app_dir` 指向需要复用的 app：

```toml
[case]
name = "orangepi5plus-smoke"
app_dir = "../../apps/starry/my-app"

[assets]
build = ["./build-assets.sh"]

[[assets.items]]
source = "path/inside/app/install-dir"
target = "/target/path/on/board"

[linux]
command = '''
cd /target/path/on/board &&
./smoke-test &&
echo APP_LINUX_DONE
'''
success_regex = ["(?m)^APP_LINUX_DONE$"]
fail_regex = ["(?i)\\bpanic(?:ked)?\\b", "(?i)not found"]
timeout = 120

[starry]
board_config = "board-orangepi-5-plus-smoke.toml"
```

常用命令：

```bash
export BOARD_PASSWORD='<password>'

# 单独上传任意文件或目录
tools/starry-board-flow.py upload-path ./local/path /remote/path \
  --board-ip 192.168.10.2

# 只构建并上传某个 case 声明的资产
tools/starry-board-flow.py upload-assets board-cases/orangepi5plus-rknn-yolo \
  --build --board-ip 192.168.10.2

# 只构建 StarryOS 并生成 FIT
tools/starry-board-flow.py build-fit --build-kernel \
  --build-config os/StarryOS/configs/board/orangepi-5-plus.toml \
  --smp 1

# 只上传 FIT
tools/starry-board-flow.py upload-fit \
  --fit tmp/axbuild/image.fit --board-ip 192.168.10.2

# 只跑 Linux 侧测试
tools/starry-board-flow.py linux-test board-cases/orangepi5plus-rknn-yolo \
  --board-ip 192.168.10.2

# 只通过 one-shot 进 StarryOS 并跑 board config 中的测试
tools/starry-board-flow.py starry-test board-cases/orangepi5plus-rknn-yolo \
  --serial /dev/tty.usbserial-0001 --board-ip 192.168.10.2

# 组合流程：上传资产、生成并上传 FIT、先 Linux 测试、再 Starry 测试
tools/starry-board-flow.py run board-cases/orangepi5plus-rknn-yolo \
  --deploy-assets --build-assets \
  --build-kernel --build-fit --deploy-fit \
  --side both \
  --fit tmp/axbuild/image.fit \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2

# 不重传资产/FIT，只重跑 Starry 侧测试
tools/starry-board-flow.py run board-cases/orangepi5plus-rknn-yolo \
  --side starry \
  --serial /dev/tty.usbserial-0001 \
  --board-ip 192.168.10.2
```

如果要在 Starry 测试结束后等待 Linux 回来，加 `--wait-linux`。默认情况下工具会
在 StarryOS 测试命令末尾追加 `reboot -f`，通过 PSCI reset 自动回 stock Linux。
由于 one-shot 标记已经在 U-Boot 启动 StarryOS 前清回 `starry_boot=0`，复位后
默认回 stock Linux。如果要改用可脚本化电源控制，可以用：

```bash
tools/starry-board-flow.py run board-cases/<case> --side starry --wait-linux \
  --power-cycle-command './path/to/power-cycle-board.sh'
```

### 已知失败形式

下面这种形式在本文场景下失败过：

```text
bootm 0x05480000
```

报错类似：

```text
bootm can't read dtb, ret=-1
```

因此不要默认依赖裸 `bootm <fit_addr>`，而应显式指定 FIT 里的 `kernel` 和 `fdt` 子镜像。

## Starry 当前看到的 rootfs

这次启动日志已经明确说明了 Starry 当前选中的根文件系统：

```text
partition 1 name=Some("bootfs") fs=None lba 61440..2158592
partition 2 name=None fs=Some(Ext4) lba 2158592..61702144
only one supported filesystem partition is available; using it as root
selected root device: disk0 partition 2 (<unnamed>, fs=ext4, lba 2158592..61702144)
```

这说明：

- `partition 1` 的名字是 `bootfs`
- `partition 2` 是 ext4
- Starry 最终把 `partition 2` 挂成了 `/`

也就是说，当前实际关系是：

- U-Boot 从 `bootfs` 读取 `image.fit`
- Starry 的 `/` 是 `partition 2` 的 ext4 rootfs
- Starry 没有再把 `bootfs` 额外挂载成 `/boot`

## 对后续传应用资产的含义

这次结果对后续板测很重要：

- `image.fit` 可以只放在 `bootfs`，供 U-Boot 读取
- 但 Starry 启动后默认看不到 `bootfs`
- 如果要让 Starry 里的应用读取资产文件，应把资产放进 Linux/Starry 共用的 ext4 rootfs，也就是当前日志里的 `partition 2`

因此：

- 内核镜像/FIT：放 `bootfs`
- Starry app 资产：放 ext4 rootfs

这也是后续把 `cargo xtask starry app board` 扩展到“自动同步资产到板子”时最应该沿用的方向。

## 当前路径的优缺点

优点：

- 不依赖当前 stock U-Boot 的 TFTP/run 自动化能力
- 不依赖 SPI 中额外刷入的 `schneid-l` U-Boot
- 利用 Linux 自身网络，`scp` 文件最直接

缺点：

- 仍是手工启动路径
- 仍依赖串口手动进入 U-Boot
- 还没有和 `cargo xtask starry app board` 集成为一键流程

## 建议的后续方向

如果后续要继续沿着这条路径演进，建议按下面顺序推进：

1. 保留本文这条“可人工兜底”的 stock U-Boot 手工链路。
2. 基于 Linux 网络侧的 `scp/rsync`，把 app 资产同步到 ext4 rootfs。
3. 再考虑把“传 FIT 到 bootfs + 传资产到 ext4 rootfs + 重启进入 U-Boot + 执行 bootm”封装成新的 `xtask` 流程。

在这之前，不建议把这条路径和旧的 `schneid-l` TFTP 文档混写在一起，否则后续很容易把两套前提条件搞混。
