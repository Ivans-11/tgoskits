// SPDX-License-Identifier: Apache-2.0
#include "state_machine.h"

#include <algorithm>
#include <cmath>
#include <cstdlib>

namespace tennis {

namespace {

constexpr int64_t kNsPerMs = 1000000;

} // namespace

const char *to_string(GameState s) {
    switch (s) {
    case GameState::CHASE_BALL: return "CHASE_BALL";
    case GameState::GRAB: return "GRAB";
    case GameState::FIND_BUCKET: return "FIND_BUCKET";
    case GameState::APPROACH_BUCKET: return "APPROACH_BUCKET";
    case GameState::DEPOSIT: return "DEPOSIT";
    }
    return "CHASE_BALL";
}

const char *to_string(PerceptionMode m) {
    return m == PerceptionMode::BALL ? "BALL" : "BUCKET";
}

static int clampi(int v, int lo, int hi) {
    return v < lo ? lo : (v > hi ? hi : v);
}

StateMachine::StateMachine(const Config &cfg)
    : cfg_(cfg), half_w_(cfg.frame_w / 2) {}

PerceptionMode StateMachine::perception_mode() const {
    switch (state_) {
    case GameState::CHASE_BALL:
    case GameState::GRAB:
        return PerceptionMode::BALL;
    default:
        return PerceptionMode::BUCKET;
    }
}

void StateMachine::enter(GameState s) {
    state_ = s;
    // Reset per-state bookkeeping on entry.
    stop_confirm_ = 0;
    align_frames_ = 0;
    state_action_started_ = false;
    state_deadline_ns_ = 0;
    bucket_confirm_ = 0;
    bucket_lost_ = 0;
}

void StateMachine::start_timed_phase(TimedPhase phase, int64_t now_ns,
                                     int duration_ms) {
    timed_phase_ = phase;
    timed_deadline_ns_ = now_ns +
                         static_cast<int64_t>(std::max(duration_ms, 0)) *
                             kNsPerMs;
}

bool StateMachine::run_timed_phase(int64_t now_ns, ControlOutput &out) {
    switch (timed_phase_) {
    case Normal: return false;
    case AlignKick:
        if (now_ns < timed_deadline_ns_) {
            out.motor_op = MotorOp::Drive;
            out.left = timed_direction_ * cfg_.align_kick_spd;
            out.right = -out.left;
            return true;
        }
        start_timed_phase(AlignKickBrake, now_ns, cfg_.align_kick_brake_ms);
        out.motor_op = MotorOp::Brake;
        return true;
    case AlignKickBrake:
        if (now_ns < timed_deadline_ns_) {
            out.motor_op = MotorOp::Brake;
            return true;
        }
        timed_phase_ = Normal;
        return false;
    case BrakeForGrab:
        if (now_ns < timed_deadline_ns_) {
            out.motor_op = MotorOp::Brake;
            return true;
        }
        timed_phase_ = Normal;
        enter(GameState::GRAB);
        out = grab(now_ns);
        return true;
    case BrakeForDeposit:
        if (now_ns < timed_deadline_ns_) {
            out.motor_op = MotorOp::Brake;
            return true;
        }
        timed_phase_ = Normal;
        enter(GameState::DEPOSIT);
        out = deposit(now_ns);
        return true;
    }
    return false;
}

// Forward speed for the far/approach zone: full speed when far, tapering to the
// brake speed as the ball fills the frame.
int StateMachine::base_speed(float area) const {
    if (area >= cfg_.area_brake) return cfg_.brake_speed;
    float t = (area - cfg_.area_far) / (cfg_.area_brake - cfg_.area_far);
    if (t < 0.f) t = 0.f;
    if (t > 1.f) t = 1.f;
    return static_cast<int>(cfg_.chase_speed_far +
                            t * (cfg_.brake_speed - cfg_.chase_speed_far));
}

ControlOutput StateMachine::step(const Detection &det, int64_t now_ns) {
    ControlOutput timed_output;
    if (run_timed_phase(now_ns, timed_output)) return timed_output;

    switch (state_) {
    case GameState::CHASE_BALL: return chase_ball(det.ball, now_ns);
    case GameState::GRAB: return grab(now_ns);
    case GameState::FIND_BUCKET: return find_bucket(det.bucket);
    case GameState::APPROACH_BUCKET:
        return approach_bucket(det.bucket, now_ns);
    case GameState::DEPOSIT: return deposit(now_ns);
    }
    return {};
}

std::optional<ControlOutput> StateMachine::tick(int64_t now_ns) {
    ControlOutput out;
    if (run_timed_phase(now_ns, out)) return out;
    if (state_ == GameState::GRAB) return grab(now_ns);
    if (state_ == GameState::DEPOSIT) return deposit(now_ns);
    return std::nullopt;
}

ControlOutput StateMachine::chase_ball(const BallObs &ball, int64_t now_ns) {
    ControlOutput out;

    if (!ball.found) {
        // Ball lost: pivot toward where it was last seen, then scan.
        ++frames_since_seen_;
        stop_confirm_ = 0;
        align_frames_ = 0;
        out.motor_op = MotorOp::Drive;
        if (frames_since_seen_ <= cfg_.search_frames) {
            int dir = last_offset_ >= 0 ? 1 : -1;
            out.left = dir * cfg_.search_pivot_spd;
            out.right = -dir * cfg_.search_pivot_spd;
        } else {
            if (++scan_counter_ >= cfg_.scan_flip_frames) {
                scan_dir_ = -scan_dir_;
                scan_counter_ = 0;
            }
            out.left = scan_dir_ * cfg_.search_pivot_spd;
            out.right = -scan_dir_ * cfg_.search_pivot_spd;
        }
        return out;
    }

    frames_since_seen_ = 0;
    const float area = ball.area_ratio;
    const int offset = static_cast<int>(ball.cx) - half_w_;
    last_offset_ = offset;
    const int stop_off = offset - cfg_.stop_center_offset;

    // Too close: back up (with a slight steer to keep the ball in view).
    if (area >= cfg_.area_reverse) {
        stop_confirm_ = 0;
        align_frames_ = 0;
        int bias = (std::abs(offset) <= cfg_.center_dead_zone)
                       ? 0
                       : (offset > 0 ? cfg_.max_turn_bias_far
                                     : -cfg_.max_turn_bias_far);
        out.motor_op = MotorOp::Drive;
        out.left = clampi(-cfg_.reverse_speed + bias, -100, 100);
        out.right = clampi(-cfg_.reverse_speed - bias, -100, 100);
        return out;
    }

    // Stop gate: ball close enough AND centred on the (off-centre) gripper
    // target for N consecutive frames. Hold active braking for a calibrated
    // interval before moving the arm, without blocking camera processing.
    if (area >= cfg_.area_stop && std::abs(stop_off) <= cfg_.stop_center_zone) {
        if (++stop_confirm_ >= cfg_.stop_confirm_cnt) {
            start_timed_phase(BrakeForGrab, now_ns, cfg_.brake_hold_ms);
        }
        out.motor_op = MotorOp::Brake;
        return out;
    }
    stop_confirm_ = 0;

    // Close in area but not yet on the off-centre target: proportional pivot to
    // align, with a stall kick if the ball stops moving in the frame.
    if (area >= cfg_.area_stop) {
        if (align_frames_ == 0) {
            align_off_min_ = align_off_max_ = offset;
        } else {
            align_off_min_ = std::min(align_off_min_, offset);
            align_off_max_ = std::max(align_off_max_, offset);
        }
        ++align_frames_;

        out.motor_op = MotorOp::Drive;
        if (align_frames_ >= cfg_.align_stall_frames &&
            (align_off_max_ - align_off_min_) < cfg_.align_stall_move_px) {
            // Hold the stronger pivot long enough to overcome static friction,
            // then actively brake before resuming proportional alignment.
            timed_direction_ = stop_off > 0 ? 1 : -1;
            start_timed_phase(AlignKick, now_ns, cfg_.align_kick_ms);
            out.left = timed_direction_ * cfg_.align_kick_spd;
            out.right = -out.left;
            align_frames_ = 0;
        } else {
            float t = std::fabs(static_cast<float>(stop_off)) /
                      static_cast<float>(half_w_);
            if (t > 1.f) t = 1.f;
            int pivot = cfg_.align_pivot_min +
                        static_cast<int>(
                            t * (cfg_.align_pivot_spd - cfg_.align_pivot_min));
            int dir = stop_off > 0 ? 1 : -1;
            out.left = dir * pivot;
            out.right = -dir * pivot;
        }
        return out;
    }
    align_frames_ = 0;

    // Normal pursuit. Turn bias is proportional to the ball's horizontal offset.
    int bias = (std::abs(offset) <= cfg_.center_dead_zone)
                   ? 0
                   : static_cast<int>(cfg_.k_turn * offset /
                                      static_cast<float>(half_w_));
    out.motor_op = MotorOp::Drive;
    if (area >= cfg_.area_near) {
        // Near: pure pivot (wheels opposite) so we never lose the ball.
        int p = clampi(bias, -cfg_.max_turn_bias_near, cfg_.max_turn_bias_near);
        out.left = p;
        out.right = -p;
    } else {
        // Far: differential drive, both wheels same direction.
        int spd = base_speed(area);
        int b = clampi(bias, -cfg_.max_turn_bias_far, cfg_.max_turn_bias_far);
        out.left = clampi(spd + b, -100, 100);
        out.right = clampi(spd - b, -100, 100);
    }
    return out;
}

ControlOutput StateMachine::grab(int64_t now_ns) {
    ControlOutput out;
    out.motor_op = MotorOp::Standby;
    if (!state_action_started_) {
        out.arm = ArmAction::Grab;
        state_action_started_ = true;
        state_deadline_ns_ = now_ns +
                             static_cast<int64_t>(
                                 std::max(cfg_.grab_settle_ms, 0)) *
                                 kNsPerMs;
    } else if (now_ns >= state_deadline_ns_) {
        enter(GameState::FIND_BUCKET);
    }
    return out;
}

ControlOutput StateMachine::find_bucket(const BucketObs &bucket) {
    ControlOutput out;
    if (bucket.found) {
        bucket_lost_ = 0;
        if (++bucket_confirm_ >= cfg_.bucket_confirm_cnt) {
            enter(GameState::APPROACH_BUCKET);
        }
        out.motor_op = MotorOp::Standby; // hold steady while confirming
    } else {
        bucket_confirm_ = 0;
        out.motor_op = MotorOp::Drive; // rotate-in-place search
        out.left = cfg_.bucket_search_spd;
        out.right = -cfg_.bucket_search_spd;
    }
    return out;
}

ControlOutput StateMachine::approach_bucket(const BucketObs &bucket,
                                            int64_t now_ns) {
    ControlOutput out;
    if (!bucket.found) {
        if (++bucket_lost_ > cfg_.bucket_lost_frames) {
            enter(GameState::FIND_BUCKET);
        }
        out.motor_op = MotorOp::Standby;
        return out;
    }
    bucket_lost_ = 0;

    const float area = bucket.area_ratio;
    if (area >= cfg_.bucket_area_deposit) {
        start_timed_phase(BrakeForDeposit, now_ns, cfg_.brake_hold_ms);
        out.motor_op = MotorOp::Brake;
        return out;
    }

    int bk_off = static_cast<int>(bucket.cx) - half_w_;
    int bias = (std::abs(bk_off) <= cfg_.center_dead_zone)
                   ? 0
                   : static_cast<int>(cfg_.bucket_k_turn * bk_off /
                                      static_cast<float>(half_w_));
    bias = clampi(bias, -cfg_.bucket_max_bias, cfg_.bucket_max_bias);
    int spd = (area >= cfg_.bucket_area_brake) ? cfg_.bucket_brake_spd
                                               : cfg_.bucket_approach_spd;
    out.motor_op = MotorOp::Drive;
    out.left = clampi(spd + bias, -100, 100);
    out.right = clampi(spd - bias, -100, 100);
    return out;
}

ControlOutput StateMachine::deposit(int64_t now_ns) {
    ControlOutput out;
    out.motor_op = MotorOp::Standby;
    if (!state_action_started_) {
        out.arm = ArmAction::Release;
        state_action_started_ = true;
        state_deadline_ns_ = now_ns +
                             static_cast<int64_t>(
                                 std::max(cfg_.release_settle_ms, 0)) *
                                 kNsPerMs;
    } else if (now_ns >= state_deadline_ns_) {
        out.arm = ArmAction::Ready;
        enter(GameState::CHASE_BALL);
        frames_since_seen_ = 1 << 20; // start the next round searching
    }
    return out;
}

} // namespace tennis
