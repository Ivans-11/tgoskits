#!/usr/bin/env python3
import argparse
import os
import platform
import sys
import time
import urllib.request


POSE_DETECTION_ONNX_URL = (
    "https://github.com/opencv/opencv_zoo/raw/main/"
    "models/person_detection_mediapipe/person_detection_mediapipe_2023mar.onnx"
)
POSE_LANDMARK_LITE_ONNX_URL = (
    "https://github.com/opencv/opencv_zoo/raw/main/"
    "models/pose_estimation_mediapipe/pose_estimation_mediapipe_2023mar.onnx"
)


def download(url, path, retries=5):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if os.path.exists(path):
        print(f"download: reuse {path}")
        return

    tmp_path = f"{path}.tmp"
    for attempt in range(1, retries + 1):
        print(f"download: {url} attempt={attempt}/{retries}", flush=True)
        try:
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            urllib.request.urlretrieve(url, tmp_path)
            os.replace(tmp_path, path)
            return
        except Exception as exc:
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            if attempt == retries:
                raise
            delay = min(2 ** attempt, 20)
            print(f"download: retry after {delay}s due to {exc}", file=sys.stderr, flush=True)
            time.sleep(delay)


def prepare_onnx_models(work_dir, skip_download):
    model_dir = os.path.join(work_dir, "onnx")
    detector = os.path.join(model_dir, "person_detection_mediapipe_2023mar.onnx")
    landmark = os.path.join(model_dir, "pose_estimation_mediapipe_2023mar.onnx")

    if not skip_download:
        download(POSE_DETECTION_ONNX_URL, detector)
        download(POSE_LANDMARK_LITE_ONNX_URL, landmark)

    if not os.path.exists(detector) or not os.path.exists(landmark):
        raise RuntimeError(f"expected OpenCV Zoo pose ONNX models under {model_dir}; rerun without --skip-download")
    return detector, landmark


def convert_onnx_to_rknn(onnx_path, rknn_path, verbose=False):
    from rknn.api import RKNN

    print(f"convert: {onnx_path} -> {rknn_path}", flush=True)
    rknn = RKNN(verbose=verbose)
    try:
        ret = rknn.config(target_platform="rk3588")
        if ret != 0:
            raise RuntimeError(f"rknn.config failed: {ret}")
        ret = rknn.load_onnx(model=onnx_path)
        if ret != 0:
            raise RuntimeError(f"rknn.load_onnx failed for {onnx_path}: {ret}")
        ret = rknn.build(do_quantization=False)
        if ret != 0:
            raise RuntimeError(f"rknn.build failed for {onnx_path}: {ret}")
        ret = rknn.export_rknn(rknn_path)
        if ret != 0:
            raise RuntimeError(f"rknn.export_rknn failed for {rknn_path}: {ret}")
    finally:
        rknn.release()


def main():
    parser = argparse.ArgumentParser(description="Convert MediaPipe Pose ONNX models to RK3588 RKNN models.")
    parser.add_argument("--work-dir", default="/tmp/mediapipe_pose", help="scratch directory")
    parser.add_argument("--output-dir", default=None, help="directory for pose_detector.rknn and pose_landmark_lite.rknn")
    parser.add_argument("--skip-download", action="store_true", help="reuse already downloaded model assets under --work-dir")
    parser.add_argument("--verbose", action="store_true", help="enable verbose RKNN-Toolkit2 conversion logs")
    args = parser.parse_args()

    repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../.."))
    default_output = os.path.join(
        repo_root,
        "apps/starry/orangepi-5-plus-uvc-rknn/rknn-mediapipe-pose-image/model",
    )
    output_dir = os.path.abspath(args.output_dir or default_output)

    if platform.machine() not in ("x86_64", "AMD64"):
        print(
            "warning: RKNN-Toolkit2 conversion is validated on native x86_64. "
            f"Current machine is {platform.machine()}; qemu-user emulation may still segfault during RKNN conversion.",
            file=sys.stderr,
        )

    detector_onnx, landmark_onnx = prepare_onnx_models(args.work_dir, args.skip_download)

    os.makedirs(output_dir, exist_ok=True)
    detector_rknn = os.path.join(output_dir, "pose_detector.rknn")
    landmark_rknn = os.path.join(output_dir, "pose_landmark_lite.rknn")

    convert_onnx_to_rknn(detector_onnx, detector_rknn, args.verbose)
    convert_onnx_to_rknn(landmark_onnx, landmark_rknn, args.verbose)

    print(f"done: {detector_rknn}")
    print(f"done: {landmark_rknn}")


if __name__ == "__main__":
    main()
