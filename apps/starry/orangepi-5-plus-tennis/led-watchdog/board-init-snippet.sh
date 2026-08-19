# StarryOS runs /etc/starry/board-init.sh once at boot.
if [ -x /home/orangepi/led-watchdog ]; then
    /home/orangepi/led-watchdog /sys/class/leds/blue_led/brightness 500 &
fi
