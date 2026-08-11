// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <algorithm>
#include <cstdlib>

namespace tennis {

struct WheelCommand {
    int left;
    int right;
};

// Lift the common translation component to the executable speed floor before
// restoring the differential component. This preserves steering bias instead
// of independently flattening both wheels to the same minimum speed.
inline WheelCommand apply_motor_dead_zone(int left, int right, int minimum) {
    left = std::clamp(left, -100, 100);
    right = std::clamp(right, -100, 100);
    minimum = std::clamp(minimum, 1, 100);

    const int base = (left + right) / 2;
    if (base != 0) {
        const int lifted_base =
            (base > 0 ? 1 : -1) * std::max(std::abs(base), minimum);
        const int lift = lifted_base - base;
        return {std::clamp(left + lift, -100, 100),
                std::clamp(right + lift, -100, 100)};
    }

    // A zero common component is an in-place turn. Each non-zero wheel still
    // needs to clear the physical dead zone.
    const auto lift_wheel = [minimum](int speed) {
        if (speed == 0) return 0;
        return (speed > 0 ? 1 : -1) *
               std::max(std::abs(speed), minimum);
    };
    return {lift_wheel(left), lift_wheel(right)};
}

} // namespace tennis
