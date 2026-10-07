// Money Printer (FishHash) nonce search for OpenCL.
//
// Written for Internet Money from the FishHash specification: the same steps as
// crates/imoney-pow/src/fishhash.rs, which is checked against Iron Fish's reference. The dataset
// is built on the CPU and copied to the card; this kernel only searches nonces.
//
// For each nonce: BLAKE3 expands (header hash, nonce) to a 64-byte seed; 32 rounds each read
// three 128-byte dataset items chosen by the running mix; the mix is folded to 32 bytes; BLAKE3
// of (seed, folded mix) is the result, valid when it does not exceed the target.

#define FNV_PRIME 0x01000193u
#define NUM_DATASET_ACCESSES 32
#define MAX_RESULTS 4

#define CHUNK_START 1u
#define CHUNK_END 2u
#define ROOT 8u

#define ROTR(x, n) rotate((uint)(x), (uint)(32 - (n)))

#define G(a, b, c, d, mx, my) \
    a = a + b + (mx);         \
    d = ROTR(d ^ a, 16);      \
    c = c + d;                \
    b = ROTR(b ^ c, 12);      \
    a = a + b + (my);         \
    d = ROTR(d ^ a, 8);       \
    c = c + d;                \
    b = ROTR(b ^ c, 7);

static inline uint fnv1(uint u, uint v) { return (u * FNV_PRIME) ^ v; }

// One BLAKE3 compression with block counter 0. Writes all 16 output words.
static void blake3_compress(const uint *cv, const uint *block, uint block_len, uint flags, uint *out)
{
    uint v0 = cv[0], v1 = cv[1], v2 = cv[2], v3 = cv[3];
    uint v4 = cv[4], v5 = cv[5], v6 = cv[6], v7 = cv[7];
    uint v8 = 0x6A09E667u, v9 = 0xBB67AE85u, v10 = 0x3C6EF372u, v11 = 0xA54FF53Au;
    uint v12 = 0u, v13 = 0u, v14 = block_len, v15 = flags;

    uint m[16];
    for (int i = 0; i < 16; i++) m[i] = block[i];

    for (int round = 0; round < 7; round++) {
        G(v0, v4, v8, v12, m[0], m[1]);
        G(v1, v5, v9, v13, m[2], m[3]);
        G(v2, v6, v10, v14, m[4], m[5]);
        G(v3, v7, v11, v15, m[6], m[7]);
        G(v0, v5, v10, v15, m[8], m[9]);
        G(v1, v6, v11, v12, m[10], m[11]);
        G(v2, v7, v8, v13, m[12], m[13]);
        G(v3, v4, v9, v14, m[14], m[15]);

        // Message permutation between rounds
        uint t[16];
        t[0] = m[2];   t[1] = m[6];   t[2] = m[3];   t[3] = m[10];
        t[4] = m[7];   t[5] = m[0];   t[6] = m[4];   t[7] = m[13];
        t[8] = m[1];   t[9] = m[11];  t[10] = m[12]; t[11] = m[5];
        t[12] = m[9];  t[13] = m[14]; t[14] = m[15]; t[15] = m[8];
        for (int i = 0; i < 16; i++) m[i] = t[i];
    }

    out[0] = v0 ^ v8;   out[1] = v1 ^ v9;   out[2] = v2 ^ v10;  out[3] = v3 ^ v11;
    out[4] = v4 ^ v12;  out[5] = v5 ^ v13;  out[6] = v6 ^ v14;  out[7] = v7 ^ v15;
    out[8] = v8 ^ cv[0];   out[9] = v9 ^ cv[1];   out[10] = v10 ^ cv[2];  out[11] = v11 ^ cv[3];
    out[12] = v12 ^ cv[4]; out[13] = v13 ^ cv[5]; out[14] = v14 ^ cv[6];  out[15] = v15 ^ cv[7];
}

static inline uint swap32(uint x)
{
    return (x >> 24) | ((x >> 8) & 0x0000ff00u) | ((x << 8) & 0x00ff0000u) | (x << 24);
}

// dataset:  32 words per item
// header:   the 8 words of the block's pre-proof-of-work hash (little-endian words of its bytes)
// target:   8 words, most significant first (big-endian words of the 32 target bytes)
// results:  [0] = number of nonces found, then (low, high) word pairs for up to MAX_RESULTS
__kernel void search(__global const uint *dataset,
                     const uint dataset_items,
                     __global const uint *header,
                     const ulong start_nonce,
                     __global const uint *target,
                     __global volatile uint *results)
{
    const uint iv[8] = {0x6A09E667u, 0xBB67AE85u, 0x3C6EF372u, 0xA54FF53Au,
                        0x510E527Fu, 0x9B05688Cu, 0x1F83D9ABu, 0x5BE0CD19u};
    const ulong nonce = start_nonce + (ulong)get_global_id(0);

    // Seed: BLAKE3 of the 40-byte input, 64 bytes of output
    uint block[16];
    for (int i = 0; i < 8; i++) block[i] = header[i];
    block[8] = (uint)nonce;
    block[9] = (uint)(nonce >> 32);
    for (int i = 10; i < 16; i++) block[i] = 0u;

    uint seed[16];
    blake3_compress(iv, block, 40u, CHUNK_START | CHUNK_END | ROOT, seed);

    // The mix starts as the seed twice
    uint mix[32];
    for (int i = 0; i < 16; i++) {
        mix[i] = seed[i];
        mix[i + 16] = seed[i];
    }

    for (int round = 0; round < NUM_DATASET_ACCESSES; round++) {
        __global const uint *item0 = dataset + (ulong)(mix[0] % dataset_items) * 32;
        __global const uint *item1 = dataset + (ulong)(mix[4] % dataset_items) * 32;
        __global const uint *item2 = dataset + (ulong)(mix[8] % dataset_items) * 32;

        uint fetch1[32];
        uint fetch2[32];
        for (int j = 0; j < 32; j++) {
            fetch1[j] = fnv1(mix[j], item1[j]);
            fetch2[j] = mix[j] ^ item2[j];
        }
        for (int j = 0; j < 16; j++) {
            ulong a = (ulong)item0[2 * j] | ((ulong)item0[2 * j + 1] << 32);
            ulong b = (ulong)fetch1[2 * j] | ((ulong)fetch1[2 * j + 1] << 32);
            ulong c = (ulong)fetch2[2 * j] | ((ulong)fetch2[2 * j + 1] << 32);
            ulong value = a * b + c;
            mix[2 * j] = (uint)value;
            mix[2 * j + 1] = (uint)(value >> 32);
        }
    }

    // Fold the 128-byte mix to 32 bytes
    uint folded[16];
    for (int i = 0; i < 8; i++) {
        uint h = fnv1(mix[4 * i], mix[4 * i + 1]);
        h = fnv1(h, mix[4 * i + 2]);
        folded[i] = fnv1(h, mix[4 * i + 3]);
    }
    for (int i = 8; i < 16; i++) folded[i] = 0u;

    // Result: BLAKE3 of the 64-byte seed followed by the 32-byte folded mix (two blocks, one chunk)
    uint chained[16];
    blake3_compress(iv, seed, 64u, CHUNK_START, chained);
    uint digest[16];
    blake3_compress(chained, folded, 32u, CHUNK_END | ROOT, digest);

    // Compare as big-endian numbers, most significant word first
    for (int i = 0; i < 8; i++) {
        uint word = swap32(digest[i]);
        if (word < target[i]) break;
        if (word > target[i]) return;
    }

    uint slot = atomic_inc(results);
    if (slot < MAX_RESULTS) {
        results[1 + 2 * slot] = (uint)nonce;
        results[2 + 2 * slot] = (uint)(nonce >> 32);
    }
}
