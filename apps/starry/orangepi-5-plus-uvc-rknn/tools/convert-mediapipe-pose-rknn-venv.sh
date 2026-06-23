#!/usr/bin/env bash
set -euo pipefail

case_dir="$(cd "$(dirname "$0")/.." && pwd)"
python_bin="${PYTHON:-python3.10}"
venv_dir="${RKNN_POSE_VENV:-/tmp/rknn-pose-venv}"

machine="$(uname -m)"
if [[ "${machine}" != "x86_64" && "${machine}" != "amd64" && "${ALLOW_NON_X86_64:-0}" != "1" ]]; then
  echo "error: RKNN-Toolkit2 conversion must run on native x86_64 Linux; current machine is ${machine}" >&2
  echo "set ALLOW_NON_X86_64=1 only if you intentionally want to try an unsupported host" >&2
  exit 1
fi

if ! command -v "${python_bin}" >/dev/null 2>&1; then
  echo "error: ${python_bin} not found; install Python 3.10 or set PYTHON=/path/to/python3.10" >&2
  exit 1
fi

"${python_bin}" -m venv "${venv_dir}"
# shellcheck disable=SC1091
source "${venv_dir}/bin/activate"

python -m pip install --upgrade pip
python -m pip install --no-deps rknn-toolkit2==2.3.2
python -m pip install \
  numpy==1.26.4 \
  protobuf==4.25.4 \
  psutil \
  ruamel.yaml \
  scipy \
  tqdm \
  opencv-python-headless==4.11.0.86 \
  fast-histogram \
  onnx==1.16.1 \
  onnxruntime
python -m pip install torch==2.4.0+cpu --index-url https://download.pytorch.org/whl/cpu

python - <<'PY'
from rknn.api import RKNN
import torch
import onnx

print("RKNN pose conversion env OK")
print("torch", torch.__version__)
print("onnx", onnx.__version__)
PY

cd "${case_dir}"
python tools/convert-mediapipe-pose-rknn.py "$@"
