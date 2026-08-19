#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define DEFAULT_LED "/sys/class/leds/blue_led/brightness"
#define DEFAULT_HALF_PERIOD_MS 500L

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static int set_led(const char *path, int on) {
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        fprintf(stderr, "led-watchdog: open %s: %s\n", path, strerror(errno));
        return -1;
    }

    const char value[] = {on ? '1' : '0', '\n'};
    ssize_t written = write(fd, value, sizeof(value));
    int saved_errno = errno;
    close(fd);
    if (written != (ssize_t)sizeof(value)) {
        errno = written < 0 ? saved_errno : EIO;
        fprintf(stderr, "led-watchdog: write %s: %s\n", path, strerror(errno));
        return -1;
    }
    return 0;
}

static void sleep_ms(long milliseconds) {
    struct timespec delay = {
        .tv_sec = milliseconds / 1000,
        .tv_nsec = (milliseconds % 1000) * 1000000L,
    };
    while (running && nanosleep(&delay, &delay) < 0 && errno == EINTR) {
    }
}

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : DEFAULT_LED;
    long half_period_ms = argc > 2 ? strtol(argv[2], NULL, 10) : DEFAULT_HALF_PERIOD_MS;
    if (half_period_ms <= 0) {
        fprintf(stderr, "usage: %s [brightness-path] [half-period-ms]\n", argv[0]);
        return 2;
    }

    signal(SIGINT, stop);
    signal(SIGTERM, stop);

    int on = 0;
    while (running) {
        on = !on;
        if (set_led(path, on) < 0) {
            sleep_ms(1000);
            continue;
        }
        sleep_ms(half_period_ms);
    }

    set_led(path, 0);
    return 0;
}
