// SPDX-License-Identifier: Apache-2.0
#include <cstdio>

#include "state_machine.h"

namespace {

bool expect(bool condition, const char *message) {
    if (!condition) std::fprintf(stderr, "FAIL: %s\n", message);
    return condition;
}

tennis::Detection ball_detection(float area, float cx) {
    tennis::Detection detection;
    detection.valid = true;
    detection.mode = tennis::PerceptionMode::BALL;
    detection.ball.found = true;
    detection.ball.area_ratio = area;
    detection.ball.cx = cx;
    return detection;
}

tennis::Detection bucket_detection(float area) {
    tennis::Detection detection;
    detection.valid = true;
    detection.mode = tennis::PerceptionMode::BUCKET;
    detection.bucket.found = true;
    detection.bucket.area_ratio = area;
    detection.bucket.cx = 320.0f;
    return detection;
}

constexpr int64_t ms(int value) {
    return static_cast<int64_t>(value) * 1000000;
}

bool reverse_takes_priority_over_grab() {
    tennis::Config config;
    config.stop_confirm_cnt = 1;
    tennis::StateMachine machine(config);
    const auto output = machine.step(
        ball_detection(
            config.area_reverse + 0.1f,
            static_cast<float>(config.frame_w / 2 + config.stop_center_offset)),
        0);
    return expect(output.motor_op == tennis::MotorOp::Drive,
                  "an excessively close ball must command reverse") &&
           expect(output.left < 0 && output.right < 0,
                  "both wheels must reverse when the ball is too close") &&
           expect(machine.state() == tennis::GameState::CHASE_BALL,
                  "reverse must not transition to grab");
}

bool grab_brake_uses_elapsed_time() {
    tennis::Config config;
    config.stop_confirm_cnt = 1;
    config.brake_hold_ms = 350;
    config.grab_settle_ms = 100;
    tennis::StateMachine machine(config);
    const auto detection = ball_detection(
        config.area_stop + 0.01f,
        static_cast<float>(config.frame_w / 2 + config.stop_center_offset));

    auto output = machine.step(detection, ms(0));
    if (!expect(output.motor_op == tennis::MotorOp::Brake,
                "grab approach must begin active braking"))
        return false;
    output = *machine.tick(ms(349));
    if (!expect(output.motor_op == tennis::MotorOp::Brake,
                "grab brake must remain active until its deadline"))
        return false;
    output = *machine.tick(ms(350));
    if (!expect(output.motor_op == tennis::MotorOp::Standby &&
                    output.arm == tennis::ArmAction::Grab,
                "grab must start only after braking completes"))
        return false;
    if (!expect(machine.state() == tennis::GameState::GRAB,
                "grab action must be represented by the GRAB state"))
        return false;
    machine.tick(ms(449));
    if (!expect(machine.state() == tennis::GameState::GRAB,
                "grab settle must use elapsed time, not frame count"))
        return false;
    machine.tick(ms(450));
    return expect(machine.state() == tennis::GameState::FIND_BUCKET,
                  "grab must finish at its elapsed-time deadline");
}

bool align_kick_has_a_timed_brake_phase() {
    tennis::Config config;
    config.align_stall_frames = 2;
    config.align_stall_move_px = 1000;
    config.align_kick_ms = 180;
    config.align_kick_brake_ms = 20;
    tennis::StateMachine machine(config);
    const auto detection = ball_detection(config.area_stop + 0.01f, 320.0f);

    machine.step(detection, ms(0));
    auto output = machine.step(detection, ms(10));
    if (!expect(output.motor_op == tennis::MotorOp::Drive &&
                    output.left == -config.align_kick_spd,
                "stalled alignment must start the strong pivot"))
        return false;
    output = *machine.tick(ms(189));
    if (!expect(output.motor_op == tennis::MotorOp::Drive,
                "align kick must remain active for 180 ms"))
        return false;
    output = *machine.tick(ms(190));
    if (!expect(output.motor_op == tennis::MotorOp::Brake,
                "align kick must end with active braking"))
        return false;
    output = *machine.tick(ms(209));
    if (!expect(output.motor_op == tennis::MotorOp::Brake,
                "post-kick brake must remain active until its deadline"))
        return false;
    if (!expect(!machine.tick(ms(210)),
                "expired kick braking must wait for fresh perception"))
        return false;
    output = machine.step(detection, ms(210));
    return expect(output.motor_op == tennis::MotorOp::Drive,
                  "alignment must resume after the kick brake deadline");
}

bool deposit_waits_for_brake_and_release() {
    tennis::Config config;
    config.stop_confirm_cnt = 1;
    config.bucket_confirm_cnt = 1;
    config.brake_hold_ms = 350;
    config.grab_settle_ms = 0;
    config.release_settle_ms = 500;
    tennis::StateMachine machine(config);
    const auto ball = ball_detection(
        config.area_stop + 0.01f,
        static_cast<float>(config.frame_w / 2 + config.stop_center_offset));

    machine.step(ball, ms(0));
    machine.tick(ms(350));
    machine.tick(ms(350));
    if (!expect(machine.state() == tennis::GameState::FIND_BUCKET,
                "test setup must reach bucket search"))
        return false;

    machine.step(bucket_detection(0.1f), ms(350));
    auto output = machine.step(bucket_detection(1.0f), ms(350));
    if (!expect(output.motor_op == tennis::MotorOp::Brake,
                "deposit approach must begin active braking"))
        return false;
    output = *machine.tick(ms(699));
    if (!expect(output.motor_op == tennis::MotorOp::Brake,
                "deposit brake must remain active until its deadline"))
        return false;
    output = *machine.tick(ms(700));
    if (!expect(output.arm == tennis::ArmAction::Release,
                "release must start after deposit braking"))
        return false;
    machine.tick(ms(1199));
    if (!expect(machine.state() == tennis::GameState::DEPOSIT,
                "deposit must wait for the release deadline"))
        return false;
    output = *machine.tick(ms(1200));
    return expect(output.arm == tennis::ArmAction::Ready &&
                      machine.state() == tennis::GameState::CHASE_BALL,
                  "arm must return home after the release deadline");
}

} // namespace

int main() {
    if (!reverse_takes_priority_over_grab()) return 1;
    if (!grab_brake_uses_elapsed_time()) return 1;
    if (!align_kick_has_a_timed_brake_phase()) return 1;
    if (!deposit_waits_for_brake_and_release()) return 1;
    std::puts("tennis_state_machine_test: OK");
    return 0;
}
