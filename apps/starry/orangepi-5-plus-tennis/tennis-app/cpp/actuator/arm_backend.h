// SPDX-License-Identifier: Apache-2.0
//
// Gripper-arm backend abstraction. The demo only needs three high-level verbs;
// the reference robot's servo poses map onto them (ready = home/open,
// grab = reach+close+lift, release = open over the bucket).
#pragma once

namespace tennis {

// Arm verbs, also used to label the per-command trace line.
enum class ArmAction {
    None,
    Grab,
    Release,
    Ready,
};

const char *to_string(ArmAction a);

class ArmBackend {
public:
    virtual ~ArmBackend() = default;
    virtual bool grab() = 0;    // close gripper on the ball and lift
    virtual bool release() = 0; // open gripper to drop into the bucket
    virtual bool ready() = 0;   // return to home/open pose
};

// Virtual backend: emits structured command lines, no hardware.
class TraceArmBackend final : public ArmBackend {
public:
    bool grab() override;
    bool release() override;
    bool ready() override;
};

} // namespace tennis
