# StarryOS LED watchdog

`led-watchdog` toggles the Orange Pi 5 Plus blue LED through StarryOS's
`/sys/class/leds/blue_led/brightness` interface. The default half-period is
500 ms, so one complete blink takes one second.

Build it natively on the board's Linux system:

```sh
./build-board-native.sh
```

Install the resulting binary as `/home/orangepi/led-watchdog`, then append
`board-init-snippet.sh` to `/etc/starry/board-init.sh`. StarryOS launches that
board hook during boot, independently of an interactive login shell.

This is a visible kernel/userspace liveness indicator. It does not reset a
hung system like a hardware watchdog timer.
