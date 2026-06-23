#!/usr/bin/env bash
set -euo pipefail

case_dir="$(cd "$(dirname "$0")" && pwd)"
cross_prefix="${CROSS_COMPILE:-aarch64-linux-gnu-}"
cc="${CC:-${cross_prefix}gcc}"
cxx="${CXX:-${cross_prefix}g++}"

command -v "${cc}" >/dev/null
command -v "${cxx}" >/dev/null

usage() {
    cat <<'EOF'
usage: build-image-runner.sh [all|yolo|pose]...

Build fixed-image RKNN runner install trees.

Targets:
  all   build YOLO and MediaPipe Pose runners (default)
  yolo  build only rknn-yolov8-image
  pose  build only rknn-mediapipe-pose-image

Before building pose, generate the RKNN model files on native x86_64 Linux:
  tools/convert-mediapipe-pose-rknn-venv.sh
EOF
}

check_pose_models() {
    local model_dir="${case_dir}/rknn-mediapipe-pose-image/model"
    local missing=0
    local model

    for model in pose_detector.rknn pose_landmark_lite.rknn; do
        if [[ ! -s "${model_dir}/${model}" ]]; then
            echo "error: missing MediaPipe Pose RKNN model: ${model_dir}/${model}" >&2
            missing=1
        fi
    done

    if [[ "${missing}" -ne 0 ]]; then
        cat >&2 <<'EOF'

Generate the pose models on a native x86_64 Linux host before building pose:
  cd apps/starry/orangepi-5-plus-uvc-rknn
  tools/convert-mediapipe-pose-rknn-venv.sh

The repository normally carries known-good .rknn files; regenerate them only
when intentionally updating the pose models.
EOF
        exit 1
    fi
}

build_project() {
    local name="$1"
    local src_dir="$2"
    local install_name="$3"
    local build_dir="${src_dir}/build-rk3588-aarch64"
    local install_dir="${src_dir}/install/rk3588_linux_aarch64/${install_name}"

    rm -rf "${build_dir}" "${install_dir}"
    mkdir -p "${build_dir}" "${install_dir}"

    cmake -S "${src_dir}" -B "${build_dir}" \
      -DCMAKE_C_COMPILER="${cc}" \
      -DCMAKE_CXX_COMPILER="${cxx}" \
      -DCMAKE_SYSTEM_NAME=Linux \
      -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
      -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_INSTALL_PREFIX="${install_dir}" \
      -DTARGET_SOC=rk3588

    cmake --build "${build_dir}" -j"$(nproc)"
    cmake --install "${build_dir}"

    echo "installed ${name}: ${install_dir}"
}

build_yolo() {
    build_project "yolov8" \
      "${case_dir}/rknn-yolov8-image" \
      "rknn_yolov8_image"
}

build_pose() {
    check_pose_models
    build_project "mediapipe-pose" \
      "${case_dir}/rknn-mediapipe-pose-image" \
      "rknn_mediapipe_pose_image"
}

if [[ "$#" -eq 0 ]]; then
    set -- all
fi

for target in "$@"; do
    case "${target}" in
        all)
            build_yolo
            build_pose
            ;;
        yolo|yolov8)
            build_yolo
            ;;
        pose|mediapipe-pose)
            build_pose
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown build target: ${target}" >&2
            usage >&2
            exit 2
            ;;
    esac
done
