// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <cstdint>
#include <mutex>
#include <string>

#include "motor_backend.h"
#include "serial_device.h"

namespace tennis {

class UartMotorBackend final : public MotorBackend {
public:
    explicit UartMotorBackend(const std::string &device, uint16_t ppr = 4680,
                              uint16_t pwm_frequency = 20000);
    ~UartMotorBackend() override;

    bool ready() const { return ready_; }
    bool drive(int left, int right) override;
    bool brake() override;
    bool standby() override;
    TelemetryResult read_wheel_rpm(WheelRpm &rpm) override;

private:
    bool send_command(uint8_t command, const uint8_t *payload, uint8_t size,
                      bool expect_ack = true);
    bool send_command_locked(uint8_t command, const uint8_t *payload,
                             uint8_t size, bool expect_ack);
    bool receive_ack(int timeout_ms);
    bool receive_frame(uint8_t &command, uint8_t *payload, uint8_t &size,
                       int timeout_ms);
    bool stop();

    SerialDevice serial_;
    // RPM reads and drive commands share one request/response stream, so the
    // complete transaction must be serialized rather than only the write.
    std::mutex transaction_mutex_;
    bool ready_ = false;
};

} // namespace tennis
