// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <string>

#include "arm_backend.h"
#include "serial_device.h"

namespace tennis {

class UartArmBackend final : public ArmBackend {
public:
    explicit UartArmBackend(const std::string &device);

    bool is_ready() const { return ready_; }
    void grab() override;
    void release() override;
    void ready() override;

private:
    void set_angle(int servo, float angle, int time_ms = 1000);
    bool send(const std::string &command);

    SerialDevice serial_;
    bool ready_ = false;
};

} // namespace tennis
