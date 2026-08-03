// SPDX-License-Identifier: Apache-2.0
#include "uart_motor_backend.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <thread>

namespace tennis {
namespace {

constexpr uint8_t kSof0 = 0xaa;
constexpr uint8_t kSof1 = 0x55;
constexpr uint8_t kCmdInit = 0x01;
constexpr uint8_t kCmdConfig = 0x02;
constexpr uint8_t kCmdSetSpeeds = 0x13;
constexpr uint8_t kCmdStop = 0x11;
constexpr uint8_t kRspAck = 0x80;

uint8_t checksum(uint8_t command, uint8_t size, const uint8_t *payload) {
    uint8_t value = command ^ size;
    for (uint8_t i = 0; i < size; ++i) value ^= payload[i];
    return value;
}

void put_be16(uint8_t *buffer, uint16_t value) {
    buffer[0] = static_cast<uint8_t>(value >> 8);
    buffer[1] = static_cast<uint8_t>(value);
}

} // namespace

UartMotorBackend::UartMotorBackend(const std::string &device, uint16_t ppr,
                                   uint16_t pwm_frequency) {
    if (!serial_.open(device)) return;
    std::this_thread::sleep_for(std::chrono::milliseconds(500));
    serial_.flush();
    if (!send_command(kCmdInit, nullptr, 0)) return;
    uint8_t config[4];
    put_be16(config, ppr);
    put_be16(config + 2, pwm_frequency);
    if (!send_command(kCmdConfig, config, sizeof(config))) return;
    ready_ = true;
}

UartMotorBackend::~UartMotorBackend() {
    if (serial_.is_open()) (void)stop();
}

bool UartMotorBackend::send_command(uint8_t command, const uint8_t *payload,
                                    uint8_t size, bool expect_ack) {
    if (size > 32) return false;
    uint8_t frame[37] = {kSof0, kSof1, command, size};
    if (size > 0) std::memcpy(frame + 4, payload, size);
    frame[4 + size] = checksum(command, size, payload);
    if (!serial_.write_all(frame, 5 + size)) return false;
    if (!expect_ack || receive_ack(500)) return true;
    std::fprintf(stderr, "TENNIS_ERROR motor command 0x%02x failed\n",
                 command);
    return false;
}

bool UartMotorBackend::receive_ack(int timeout_ms) {
    uint8_t byte = 0;
    int remaining = timeout_ms;
    while (remaining > 0) {
        if (!serial_.read_byte(byte, 10)) {
            remaining -= 10;
            continue;
        }
        if (byte != kSof0 || !serial_.read_byte(byte, 10) || byte != kSof1)
            continue;
        uint8_t command = 0;
        uint8_t size = 0;
        if (!serial_.read_byte(command, 50) || !serial_.read_byte(size, 50) ||
            size > 32)
            return false;
        uint8_t payload[32]{};
        for (uint8_t i = 0; i < size; ++i) {
            if (!serial_.read_byte(payload[i], 50)) return false;
        }
        uint8_t received = 0;
        if (!serial_.read_byte(received, 50)) return false;
        return received == checksum(command, size, payload) &&
               command == kRspAck;
    }
    std::fprintf(stderr, "TENNIS_ERROR motor controller ACK timed out\n");
    return false;
}

bool UartMotorBackend::drive(int left, int right) {
    const auto left_speed = static_cast<int16_t>(std::clamp(left, -100, 100));
    const auto right_speed = static_cast<int16_t>(std::clamp(right, -100, 100));
    uint8_t payload[4];
    put_be16(payload, static_cast<uint16_t>(left_speed));
    put_be16(payload + 2, static_cast<uint16_t>(right_speed));
    return send_command(kCmdSetSpeeds, payload, sizeof(payload), false);
}

bool UartMotorBackend::brake() {
    return stop();
}

bool UartMotorBackend::standby() {
    return stop();
}

bool UartMotorBackend::stop() {
    const uint8_t all_motors = 2;
    return send_command(kCmdStop, &all_motors, 1, false);
}

} // namespace tennis
