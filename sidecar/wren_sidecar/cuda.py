"""Make the CUDA 12 cuBLAS and cuDNN 9 DLLs from the nvidia-*-cu12 wheels findable on Windows.

The wheels put their DLLs under site-packages/nvidia/<lib>/bin, which Windows does not search.
CTranslate2 loads them by name, so each folder goes on the DLL search path *and* on PATH, before
faster_whisper (and so ctranslate2) is imported. Same recipe as the benchmark venv; written for Wren.

A missing cuDNN sub-library makes CTranslate2 abort the whole process at the first decode (not raise),
so `cuda_problem()` checks the files up front and the caller goes straight to the CPU model instead.
"""
from __future__ import annotations

import ctypes
import os
import sys
from pathlib import Path

# The DLLs CTranslate2 4.x needs for float16/int8 decoding with cuDNN 9.
REQUIRED_DLLS = {
    "cublas": ("cublas64_12.dll", "cublasLt64_12.dll"),
    "cudnn": ("cudnn64_9.dll", "cudnn_ops64_9.dll", "cudnn_cnn64_9.dll"),
}

_added: list[str] = []


def nvidia_root() -> Path:
    return Path(sys.prefix) / "Lib" / "site-packages" / "nvidia"


def add_dll_directories() -> list[str]:
    """Idempotent. Returns the folders added."""
    if _added:
        return list(_added)
    for sub in REQUIRED_DLLS:
        bin_dir = nvidia_root() / sub / "bin"
        if bin_dir.is_dir():
            os.add_dll_directory(str(bin_dir))
            os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
            _added.append(str(bin_dir))
    return list(_added)


def cuda_problem() -> str | None:
    """None if CUDA looks usable, else a short reason (no paths that could leak anything personal)."""
    for sub, names in REQUIRED_DLLS.items():
        bin_dir = nvidia_root() / sub / "bin"
        for name in names:
            if not (bin_dir / name).is_file():
                return f"{name} is missing from the runtime"
    add_dll_directories()
    try:
        import ctranslate2

        if ctranslate2.get_cuda_device_count() < 1:
            return "no CUDA GPU found"
    except Exception as e:  # noqa: BLE001 - any failure here means "no CUDA"
        return f"CUDA check failed ({type(e).__name__})"
    return None


class _NvmlMemory(ctypes.Structure):
    _fields_ = [("total", ctypes.c_ulonglong), ("free", ctypes.c_ulonglong), ("used", ctypes.c_ulonglong)]


def gpu_used_mb(index: int = 0) -> int | None:
    """Total GPU memory in use (MB) via NVML, or None. Per-process numbers are N/A under WDDM, so the
    sidecar reports the rise across model load + warm-up."""
    try:
        nvml = ctypes.WinDLL("nvml.dll")
    except OSError:
        return None
    try:
        if nvml.nvmlInit_v2() != 0:
            return None
        handle = ctypes.c_void_p()
        if nvml.nvmlDeviceGetHandleByIndex_v2(index, ctypes.byref(handle)) != 0:
            return None
        mem = _NvmlMemory()
        if nvml.nvmlDeviceGetMemoryInfo(handle, ctypes.byref(mem)) != 0:
            return None
        return int(mem.used // (1024 * 1024))
    except Exception:  # noqa: BLE001
        return None
    finally:
        try:
            nvml.nvmlShutdown()
        except Exception:  # noqa: BLE001
            pass
