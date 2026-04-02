"""NumPy matrix multiply benchmark — reports GFLOPS."""
import os
import time
import numpy as np

MATRIX_SIZE = int(os.environ.get("MATRIX_SIZE", "2000"))

print(f"STARTING matrix_size={MATRIX_SIZE}", flush=True)

a = np.random.randn(MATRIX_SIZE, MATRIX_SIZE).astype(np.float64)
b = np.random.randn(MATRIX_SIZE, MATRIX_SIZE).astype(np.float64)

# Warmup
_ = a @ b

# Timed run
start = time.perf_counter()
c = a @ b
elapsed = time.perf_counter() - start

# 2 * N^3 FLOPs for matrix multiply
flops = 2.0 * MATRIX_SIZE ** 3
gflops = flops / elapsed / 1e9

print(f"RESULT gflops={gflops:.2f} elapsed={elapsed:.3f}s size={MATRIX_SIZE}", flush=True)
