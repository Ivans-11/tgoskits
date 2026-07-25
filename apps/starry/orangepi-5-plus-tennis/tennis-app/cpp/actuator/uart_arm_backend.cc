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

void UartArmBackend::set_angle(int servo, float angle, int time_ms) {
    angle = std::clamp(angle, 0.0f, kAngleMax);
    const int pulse = std::clamp(
        static_cast<int>(500.0f + angle / kAngleMax * 2000.0f), 500, 2500);
    char command[32];
    std::snprintf(command, sizeof(command), "#%03dP%04dT%d!", servo, pulse,
                  time_ms);
    send(command);
}

void UartArmBackend::grab() {
    set_angle(0, kServo0Grab);
    set_angle(1, kServo1Grab);
    set_angle(2, kOpen);
    std::this_thread::sleep_for(std::chrono::milliseconds(1500));
    set_angle(2, kClosed);
    std::this_thread::sleep_for(std::chrono::milliseconds(1000));
    set_angle(0, kServo0Lift);
    set_angle(1, kServo1Lift);
    std::this_thread::sleep_for(std::chrono::milliseconds(1200));
}

void UartArmBackend::release() {
    set_angle(2, kOpen);
}

void UartArmBackend::ready() {
    set_angle(0, kServo0Ready);
    set_angle(1, kServo1Ready);
    set_angle(2, kOpen);
}

} // namespace tennis
