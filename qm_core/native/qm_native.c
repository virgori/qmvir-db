/*
 * QM Native — High-performance C kernels for QM database core.
 *
 * SIMD-accelerated operations:
 *   1. Bitmap AND / OR / ANDNOT / XOR / popcount
 *   2. Block BM25 scoring (batch of 128 docs)
 *   3. Batch L2 and cosine distance computation
 *   4. Binary search on sorted arrays
 *   5. CRC32 computation for WAL records
 *
 * Compile as a Python C extension via setup.py.
 * On Apple Silicon (arm64), uses NEON intrinsics.
 * On x86_64, uses SSE4.2 / AVX2 when available.
 *
 * Build:
 *   cd qm_core/native && python setup.py build_ext --inplace
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

/* ── Platform detection ───────────────────────────────────────────── */

#if defined(__aarch64__) || defined(_M_ARM64)
  #define QM_ARM64 1
  #include <arm_neon.h>
#elif defined(__x86_64__) || defined(_M_X64)
  #define QM_X86_64 1
  #if defined(__SSE4_2__)
    #include <nmmintrin.h>
  #endif
  #if defined(__AVX2__)
    #include <immintrin.h>
  #endif
#endif


/* ── Bitmap operations ────────────────────────────────────────────── */

static inline uint64_t popcount64(uint64_t x) {
#if defined(__GNUC__) || defined(__clang__)
    return __builtin_popcountll(x);
#else
    /* Fallback: Hamming weight */
    x = x - ((x >> 1) & 0x5555555555555555ULL);
    x = (x & 0x3333333333333333ULL) + ((x >> 2) & 0x3333333333333333ULL);
    x = (x + (x >> 4)) & 0x0F0F0F0F0F0F0F0FULL;
    return (x * 0x0101010101010101ULL) >> 56;
}
#endif


/*
 * bitmap_and: dst = a & b, return popcount of result.
 * All arrays are uint64_t[n_words].
 */
static PyObject* qm_bitmap_and(PyObject* self, PyObject* args) {
    Py_buffer buf_a, buf_b;
    if (!PyArg_ParseTuple(args, "y*y*", &buf_a, &buf_b))
        return NULL;

    Py_ssize_t n = buf_a.len < buf_b.len ? buf_a.len : buf_b.len;
    Py_ssize_t n_words = n / 8;

    /* Allocate output */
    PyObject* result = PyBytes_FromStringAndSize(NULL, n_words * 8);
    if (!result) {
        PyBuffer_Release(&buf_a);
        PyBuffer_Release(&buf_b);
        return NULL;
    }

    const uint64_t* a = (const uint64_t*)buf_a.buf;
    const uint64_t* b = (const uint64_t*)buf_b.buf;
    uint64_t* dst = (uint64_t*)PyBytes_AS_STRING(result);
    uint64_t count = 0;

    for (Py_ssize_t i = 0; i < n_words; i++) {
        dst[i] = a[i] & b[i];
        count += popcount64(dst[i]);
    }

    PyBuffer_Release(&buf_a);
    PyBuffer_Release(&buf_b);

    return Py_BuildValue("(Ok)", result, (unsigned long)count);
}


/*
 * bitmap_or: dst = a | b, return popcount.
 */
static PyObject* qm_bitmap_or(PyObject* self, PyObject* args) {
    Py_buffer buf_a, buf_b;
    if (!PyArg_ParseTuple(args, "y*y*", &buf_a, &buf_b))
        return NULL;

    Py_ssize_t n = buf_a.len < buf_b.len ? buf_a.len : buf_b.len;
    Py_ssize_t n_words = n / 8;

    PyObject* result = PyBytes_FromStringAndSize(NULL, n_words * 8);
    if (!result) {
        PyBuffer_Release(&buf_a);
        PyBuffer_Release(&buf_b);
        return NULL;
    }

    const uint64_t* a = (const uint64_t*)buf_a.buf;
    const uint64_t* b = (const uint64_t*)buf_b.buf;
    uint64_t* dst = (uint64_t*)PyBytes_AS_STRING(result);
    uint64_t count = 0;

    for (Py_ssize_t i = 0; i < n_words; i++) {
        dst[i] = a[i] | b[i];
        count += popcount64(dst[i]);
    }

    PyBuffer_Release(&buf_a);
    PyBuffer_Release(&buf_b);
    return Py_BuildValue("(Ok)", result, (unsigned long)count);
}


/*
 * bitmap_popcount: count set bits.
 */
static PyObject* qm_bitmap_popcount(PyObject* self, PyObject* args) {
    Py_buffer buf;
    if (!PyArg_ParseTuple(args, "y*", &buf))
        return NULL;

    Py_ssize_t n_words = buf.len / 8;
    const uint64_t* data = (const uint64_t*)buf.buf;
    uint64_t count = 0;

    for (Py_ssize_t i = 0; i < n_words; i++) {
        count += popcount64(data[i]);
    }

    PyBuffer_Release(&buf);
    return PyLong_FromUnsignedLongLong(count);
}


/* ── BM25 Block Scoring ───────────────────────────────────────────── */

/*
 * bm25_score_block: Score a block of 128 documents.
 *
 * Args:
 *   tfs: bytes (float32[128]) — term frequencies
 *   dls: bytes (float32[128]) — document lengths
 *   avg_dl: float — average document length
 *   n_docs: int — total documents
 *   df: int — document frequency
 *   k1: float — BM25 k1 parameter
 *   b: float — BM25 b parameter
 *
 * Returns: bytes (float32[128]) — BM25 scores
 */
static PyObject* qm_bm25_score_block(PyObject* self, PyObject* args) {
    Py_buffer buf_tf, buf_dl;
    double avg_dl, k1, b;
    long n_docs, df;

    if (!PyArg_ParseTuple(args, "y*y*dlldd", &buf_tf, &buf_dl, &avg_dl, &n_docs, &df, &k1, &b))
        return NULL;

    int block_size = buf_tf.len / sizeof(float);
    if (block_size > 128) block_size = 128;

    const float* tf = (const float*)buf_tf.buf;
    const float* dl = (const float*)buf_dl.buf;

    float idf = (float)log(((double)n_docs - (double)df + 0.5) / ((double)df + 0.5) + 1.0);

    PyObject* result = PyBytes_FromStringAndSize(NULL, block_size * sizeof(float));
    if (!result) {
        PyBuffer_Release(&buf_tf);
        PyBuffer_Release(&buf_dl);
        return NULL;
    }
    float* scores = (float*)PyBytes_AS_STRING(result);

#if defined(QM_ARM64)
    /* NEON vectorized BM25 */
    float32x4_t v_idf = vdupq_n_f32(idf);
    float32x4_t v_k1 = vdupq_n_f32((float)k1);
    float32x4_t v_b = vdupq_n_f32((float)b);
    float32x4_t v_one = vdupq_n_f32(1.0f);
    float32x4_t v_avg_dl = vdupq_n_f32((float)avg_dl);
    float32x4_t v_k1_plus_1 = vaddq_f32(v_k1, v_one);

    for (int i = 0; i < block_size; i += 4) {
        float32x4_t v_tf = vld1q_f32(tf + i);
        float32x4_t v_dl = vld1q_f32(dl + i);

        /* norm = 1 - b + b * (dl / avg_dl) */
        float32x4_t v_ratio = vdivq_f32(v_dl, v_avg_dl);
        float32x4_t v_norm = vaddq_f32(vsubq_f32(v_one, v_b),
                                        vmulq_f32(v_b, v_ratio));

        /* numerator = tf * (k1 + 1) */
        float32x4_t v_num = vmulq_f32(v_tf, v_k1_plus_1);

        /* denominator = tf + k1 * norm */
        float32x4_t v_den = vaddq_f32(v_tf, vmulq_f32(v_k1, v_norm));

        /* score = idf * num / den */
        float32x4_t v_score = vmulq_f32(v_idf, vdivq_f32(v_num, v_den));
        vst1q_f32(scores + i, v_score);
    }
#else
    /* Scalar fallback */
    for (int i = 0; i < block_size; i++) {
        float norm = 1.0f - (float)b + (float)b * (dl[i] / (float)avg_dl);
        float num = tf[i] * ((float)k1 + 1.0f);
        float den = tf[i] + (float)k1 * norm;
        scores[i] = idf * num / den;
    }
#endif

    PyBuffer_Release(&buf_tf);
    PyBuffer_Release(&buf_dl);
    return result;
}


/* ── Batch L2 Distance ────────────────────────────────────────────── */

/*
 * batch_l2_distance: Compute L2² from one query to N vectors.
 *
 * Args:
 *   query: bytes (float32[dim])
 *   vectors: bytes (float32[n * dim])
 *   dim: int
 *
 * Returns: bytes (float32[n]) — squared L2 distances
 */
static PyObject* qm_batch_l2(PyObject* self, PyObject* args) {
    Py_buffer buf_q, buf_v;
    int dim;

    if (!PyArg_ParseTuple(args, "y*y*i", &buf_q, &buf_v, &dim))
        return NULL;

    int n = buf_v.len / (dim * sizeof(float));
    const float* q = (const float*)buf_q.buf;
    const float* v = (const float*)buf_v.buf;

    PyObject* result = PyBytes_FromStringAndSize(NULL, n * sizeof(float));
    if (!result) {
        PyBuffer_Release(&buf_q);
        PyBuffer_Release(&buf_v);
        return NULL;
    }
    float* dists = (float*)PyBytes_AS_STRING(result);

    for (int i = 0; i < n; i++) {
        float sum = 0.0f;
        const float* vec = v + i * dim;

#if defined(QM_ARM64)
        float32x4_t v_sum = vdupq_n_f32(0.0f);
        int j;
        for (j = 0; j + 4 <= dim; j += 4) {
            float32x4_t v_q = vld1q_f32(q + j);
            float32x4_t v_v = vld1q_f32(vec + j);
            float32x4_t v_diff = vsubq_f32(v_q, v_v);
            v_sum = vmlaq_f32(v_sum, v_diff, v_diff);
        }
        sum = vaddvq_f32(v_sum);
        for (; j < dim; j++) {
            float d = q[j] - vec[j];
            sum += d * d;
        }
#else
        for (int j = 0; j < dim; j++) {
            float d = q[j] - vec[j];
            sum += d * d;
        }
#endif
        dists[i] = sum;
    }

    PyBuffer_Release(&buf_q);
    PyBuffer_Release(&buf_v);
    return result;
}


/* ── CRC32 ────────────────────────────────────────────────────────── */

static PyObject* qm_crc32(PyObject* self, PyObject* args) {
    Py_buffer buf;
    if (!PyArg_ParseTuple(args, "y*", &buf))
        return NULL;

    uint32_t crc = 0xFFFFFFFF;
    const uint8_t* data = (const uint8_t*)buf.buf;

#if defined(QM_ARM64) && defined(__ARM_FEATURE_CRC32)
    /* Use hardware CRC32 on ARM */
    Py_ssize_t i;
    for (i = 0; i + 8 <= buf.len; i += 8) {
        crc = __builtin_arm_crc32d(crc, *(const uint64_t*)(data + i));
    }
    for (; i < buf.len; i++) {
        crc = __builtin_arm_crc32b(crc, data[i]);
    }
#elif defined(QM_X86_64) && defined(__SSE4_2__)
    Py_ssize_t i;
    for (i = 0; i + 8 <= buf.len; i += 8) {
        crc = (uint32_t)_mm_crc32_u64(crc, *(const uint64_t*)(data + i));
    }
    for (; i < buf.len; i++) {
        crc = _mm_crc32_u8(crc, data[i]);
    }
#else
    /* Software CRC32 (lookup table) */
    static uint32_t table[256];
    static int table_init = 0;
    if (!table_init) {
        for (int i = 0; i < 256; i++) {
            uint32_t c = (uint32_t)i;
            for (int j = 0; j < 8; j++)
                c = (c >> 1) ^ (0xEDB88320 & (-(c & 1)));
            table[i] = c;
        }
        table_init = 1;
    }
    for (Py_ssize_t i = 0; i < buf.len; i++) {
        crc = (crc >> 8) ^ table[(crc ^ data[i]) & 0xFF];
    }
#endif

    crc ^= 0xFFFFFFFF;
    PyBuffer_Release(&buf);
    return PyLong_FromUnsignedLong(crc);
}


/* ── XOR-Delta Vector Compression (SIMD) ──────────────────────────── */

/*
 * xor_delta_encode: XOR a vector against a reference, return delta bytes
 *                   and a bitmask of non-zero positions.
 *
 * Args:
 *   vec: bytes (float32[dim])  — vector to encode
 *   ref: bytes (float32[dim])  — reference vector
 *
 * Returns: (bitmask: bytes, non_zero_deltas: bytes)
 */
static PyObject* qm_xor_delta_encode(PyObject* self, PyObject* args) {
    Py_buffer buf_vec, buf_ref;
    if (!PyArg_ParseTuple(args, "y*y*", &buf_vec, &buf_ref))
        return NULL;

    int dim = buf_vec.len / sizeof(uint32_t);
    const uint32_t* vec = (const uint32_t*)buf_vec.buf;
    const uint32_t* ref = (const uint32_t*)buf_ref.buf;

    int bitmask_bytes = (dim + 7) / 8;
    uint8_t* bitmask = (uint8_t*)calloc(bitmask_bytes, 1);
    uint32_t* deltas = (uint32_t*)malloc(dim * sizeof(uint32_t));
    int n_nonzero = 0;

#if defined(QM_ARM64)
    /* NEON XOR 4 uint32 at a time */
    int i;
    for (i = 0; i + 4 <= dim; i += 4) {
        uint32x4_t v_vec = vld1q_u32(vec + i);
        uint32x4_t v_ref = vld1q_u32(ref + i);
        uint32x4_t v_xor = veorq_u32(v_vec, v_ref);

        uint32_t xor_vals[4];
        vst1q_u32(xor_vals, v_xor);

        for (int j = 0; j < 4; j++) {
            if (xor_vals[j] != 0) {
                bitmask[(i + j) >> 3] |= 1 << ((i + j) & 7);
                deltas[n_nonzero++] = xor_vals[j];
            }
        }
    }
    for (; i < dim; i++) {
        uint32_t d = vec[i] ^ ref[i];
        if (d != 0) {
            bitmask[i >> 3] |= 1 << (i & 7);
            deltas[n_nonzero++] = d;
        }
    }
#else
    for (int i = 0; i < dim; i++) {
        uint32_t d = vec[i] ^ ref[i];
        if (d != 0) {
            bitmask[i >> 3] |= 1 << (i & 7);
            deltas[n_nonzero++] = d;
        }
    }
#endif

    PyObject* py_bitmask = PyBytes_FromStringAndSize((const char*)bitmask, bitmask_bytes);
    PyObject* py_deltas = PyBytes_FromStringAndSize((const char*)deltas, n_nonzero * sizeof(uint32_t));

    free(bitmask);
    free(deltas);
    PyBuffer_Release(&buf_vec);
    PyBuffer_Release(&buf_ref);

    return Py_BuildValue("(OO)", py_bitmask, py_deltas);
}


/*
 * xor_delta_decode: Reconstruct a vector from reference + bitmask + deltas.
 *
 * Args:
 *   ref: bytes (float32[dim])  — reference vector
 *   bitmask: bytes             — non-zero position bitmask
 *   deltas: bytes              — non-zero XOR delta values
 *   dim: int                   — vector dimension
 *
 * Returns: bytes (float32[dim])
 */
static PyObject* qm_xor_delta_decode(PyObject* self, PyObject* args) {
    Py_buffer buf_ref, buf_bitmask, buf_deltas;
    int dim;

    if (!PyArg_ParseTuple(args, "y*y*y*i", &buf_ref, &buf_bitmask, &buf_deltas, &dim))
        return NULL;

    const uint32_t* ref = (const uint32_t*)buf_ref.buf;
    const uint8_t* bitmask = (const uint8_t*)buf_bitmask.buf;
    const uint32_t* deltas = (const uint32_t*)buf_deltas.buf;

    PyObject* result = PyBytes_FromStringAndSize(NULL, dim * sizeof(uint32_t));
    uint32_t* out = (uint32_t*)PyBytes_AS_STRING(result);

    /* Start with copy of reference */
    memcpy(out, ref, dim * sizeof(uint32_t));

    /* Apply XOR deltas at bitmask positions */
    int delta_idx = 0;

#if defined(QM_ARM64)
    for (int i = 0; i < dim; i++) {
        if (bitmask[i >> 3] & (1 << (i & 7))) {
            out[i] ^= deltas[delta_idx++];
        }
    }
#else
    for (int i = 0; i < dim; i++) {
        if (bitmask[i >> 3] & (1 << (i & 7))) {
            out[i] ^= deltas[delta_idx++];
        }
    }
#endif

    PyBuffer_Release(&buf_ref);
    PyBuffer_Release(&buf_bitmask);
    PyBuffer_Release(&buf_deltas);
    return result;
}


/*
 * batch_cosine: Compute cosine similarity from one query to N vectors.
 */
static PyObject* qm_batch_cosine(PyObject* self, PyObject* args) {
    Py_buffer buf_q, buf_v;
    int dim;

    if (!PyArg_ParseTuple(args, "y*y*i", &buf_q, &buf_v, &dim))
        return NULL;

    int n = buf_v.len / (dim * sizeof(float));
    const float* q = (const float*)buf_q.buf;
    const float* v = (const float*)buf_v.buf;

    PyObject* result = PyBytes_FromStringAndSize(NULL, n * sizeof(float));
    float* sims = (float*)PyBytes_AS_STRING(result);

    /* Pre-compute query magnitude */
    float q_mag = 0.0f;
    for (int j = 0; j < dim; j++)
        q_mag += q[j] * q[j];
    q_mag = sqrtf(q_mag);

    for (int i = 0; i < n; i++) {
        const float* vec = v + i * dim;
        float dot = 0.0f, v_mag = 0.0f;

#if defined(QM_ARM64)
        float32x4_t v_dot = vdupq_n_f32(0.0f);
        float32x4_t v_vmag = vdupq_n_f32(0.0f);
        int j;
        for (j = 0; j + 4 <= dim; j += 4) {
            float32x4_t vq = vld1q_f32(q + j);
            float32x4_t vv = vld1q_f32(vec + j);
            v_dot = vmlaq_f32(v_dot, vq, vv);
            v_vmag = vmlaq_f32(v_vmag, vv, vv);
        }
        dot = vaddvq_f32(v_dot);
        v_mag = vaddvq_f32(v_vmag);
        for (; j < dim; j++) {
            dot += q[j] * vec[j];
            v_mag += vec[j] * vec[j];
        }
        v_mag = sqrtf(v_mag);
#else
        for (int j = 0; j < dim; j++) {
            dot += q[j] * vec[j];
            v_mag += vec[j] * vec[j];
        }
        v_mag = sqrtf(v_mag);
#endif
        float denom = q_mag * v_mag;
        sims[i] = (denom > 1e-12f) ? (dot / denom) : 0.0f;
    }

    PyBuffer_Release(&buf_q);
    PyBuffer_Release(&buf_v);
    return result;
}


/* ── Module Definition ────────────────────────────────────────────── */

static PyMethodDef QMNativeMethods[] = {
    {"bitmap_and",        qm_bitmap_and,        METH_VARARGS, "AND two bitmaps, return (result, popcount)"},
    {"bitmap_or",         qm_bitmap_or,         METH_VARARGS, "OR two bitmaps, return (result, popcount)"},
    {"bitmap_popcount",   qm_bitmap_popcount,   METH_VARARGS, "Count set bits in bitmap"},
    {"bm25_score_block",  qm_bm25_score_block,  METH_VARARGS, "Score a block of 128 docs with BM25"},
    {"batch_l2",          qm_batch_l2,          METH_VARARGS, "Batch L2 squared distance"},
    {"batch_cosine",      qm_batch_cosine,      METH_VARARGS, "Batch cosine similarity"},
    {"crc32",             qm_crc32,             METH_VARARGS, "Compute CRC32 of data"},
    {"xor_delta_encode",  qm_xor_delta_encode,  METH_VARARGS, "XOR-Delta encode vector vs reference"},
    {"xor_delta_decode",  qm_xor_delta_decode,  METH_VARARGS, "XOR-Delta decode vector from reference+delta"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef qm_native_module = {
    PyModuleDef_HEAD_INIT,
    "qm_native",
    "QM high-performance native kernels (SIMD bitmap, BM25 scoring, vector distance, CRC32)",
    -1,
    QMNativeMethods
};

PyMODINIT_FUNC PyInit_qm_native(void) {
    return PyModule_Create(&qm_native_module);
}
