// SPDX-License-Identifier: Apache-2.0
#include "uart_arm_backend.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <thread>

namespace tennis {
namespace {

constexpr float kAngleMax = 270.0f;
constexpr float kOpen = 210.0f;
constexpr float kClosed = 120.0f;
constexpr float kServo0Ready = 150.0f;
constexpr float kServo1Ready = 100.0f;
constexpr float kServo0Grab = 221.0f;
constexpr float kServo1Grab = 64.0f;
constexpr float kServo0Lift = 150.0f;
constexpr float kServo1Lift = 110.0f;

} // namespace

UartArmBackend::UartArmBackend(const std::string &device) {
    ready_ = serial_.open(device, 115200, false);
}

bool UartArmBackend::send(const std::string &command) {
    if (serial_.write_all(command.data(), command.size()) && serial_.drain())
        return true;
    std::fprintf(stderr, "TENNIS_ERROR arm command failed: %s\n",
                 command.c_str());
    return false;
}

bool UartArmBackend::set_angle(int servo, float angle, int time_ms) {
    angle = std::clamp(angle, 0.0f, kAngleMax);
    const int pulse = std::clamp(
        static_cast<int>(500.0f + angle / kAngleMax * 2000.0f), 500, 2500);
    char command[32];
    std::snprintf(command, sizeof(command), "#%03dP%04dT%d!", servo, pulse,
                  time_ms);
    return send(command);
}

bool UartArmBackend::grab() {
    if (!set_angle(0, kServo0Grab) || !set_angle(1, kServo1Grab) ||
        !set_angle(2, kOpen))
        return false;
    std::this_thread::sleep_for(std::chrono::milliseconds(1500));
    if (!set_angle(2, kClosed)) return false;
    std::this_thread::sleep_for(std::chrono::milliseconds(1000));
    if (!set_angle(0, kServo0Lift) || !set_angle(1, kServo1Lift)) return false;
    std::this_thread::sleep_for(std::chrono::milliseconds(1200));
    return true;
}

bool UartArmBackend::release() {
    return set_angle(2, kOpen);
}

bool UartArmBackend::ready() {
    const bool servo0 = set_angle(0, kServo0Ready);
    const bool servo1 = set_angle(1, kServo1Ready);
    return set_angle(2, kOpen) && servo0 && servo1;
}

} // namespace tennis
