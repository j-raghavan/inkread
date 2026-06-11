/*
 * ebc_jni.c — native /dev/ebc probe for the RR19-FR4b pen-latency spike (Route 3).
 *
 * CLEAN-ROOM (RR18). The EBC ioctl ABI below is reimplemented SOLELY from the public,
 * GPL Rockchip `ebc-dev` kernel UAPI header — a documented contract — NOT from any
 * decompiled Ratta/Onyx code. Sources (verbatim #defines/struct/enum taken from):
 *
 *   Rockchip BSP (canonical, RK356x Android — matches this device's kernel lineage):
 *     github.com/rockchip-linux/kernel  .../drivers/gpu/drm/rockchip/ebc-dev/ebc_dev.h
 *   smaeul/linux (mainline DRM port; enum values >=13 DIFFER — see note below):
 *     github.com/smaeul/linux           .../drivers/gpu/drm/rockchip/ebc-dev/ebc_dev.h
 *   Buffer/mmap-offset model confirmation (clean-room RE of the .so, read for the *flow*
 *   only — NOT copied): github.com/Ralim/ebc-dev-reverse-engineering done/ebc_dev_v8.c
 *
 * ABI facts used here (HIGH confidence — two header sources + RE agree):
 *  - The driver uses RAW integer ioctl request codes 0x7000..0x7007 (NOT _IOR/_IOW; no
 *    magic, no size/dir encoding). Pass the bare numbers.
 *  - The only struct crossing the boundary is `ebc_buf_info`: 11 ints = 44 bytes (0x2c).
 *  - mmap maps the WHOLE framebuffer CMA region from offset 0; `ebc_buf_info.offset` is a
 *    byte offset into that region.
 *
 * EPD waveform enum: this device is RK3566 / Ratta (downstream of rockchip-linux BSP), so
 * we use the BSP enum where EPD_A2=12, EPD_PART_GC16=7, EPD_FULL_GC16=2. Values 0..12 are
 * IDENTICAL across both forks; only >=13 diverge — so A2 (=12) and the PART/FULL GC modes
 * we use are fork-agnostic. (MEDIUM-HIGH confidence the vendor kernel kept the BSP enum;
 * flagged in the report. We deliberately avoid mode values >=13.)
 */

#include <jni.h>
#include <android/log.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define TAG "PenSpike-ebc"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, TAG, __VA_ARGS__)

/* ---- Rockchip ebc-dev UAPI (clean-room, from the GPL header — see file banner) ---- */

/* Raw integer ioctl request codes (the driver does NOT use _IO macros). */
#define EBC_GET_BUFFER       (0x7000)
#define EBC_SEND_BUFFER      (0x7001)
#define EBC_GET_BUFFER_INFO  (0x7002)
#define EBC_SET_FULL_MODE_NUM (0x7003)
#define EBC_ENABLE_OVERLAY   (0x7004)
#define EBC_DISABLE_OVERLAY  (0x7005)
#define EBC_GET_OSD_BUFFER   (0x7006)
#define EBC_SEND_OSD_BUFFER  (0x7007)

/* enum panel_refresh_mode (BSP fork; values 0..12 are fork-stable). */
enum panel_refresh_mode {
    EPD_AUTO       = 0,
    EPD_OVERLAY    = 1,
    EPD_FULL_GC16  = 2,
    EPD_PART_GC16  = 7,
    EPD_A2         = 12, /* fast/handwriting waveform — fork-agnostic at 12 */
};

/* struct ebc_buf_info — 11 ints, 44 bytes (0x2c). Field order per the GPL header. */
struct ebc_buf_info {
    int32_t offset;
    int32_t epd_mode;
    int32_t height;
    int32_t width;
    int32_t panel_color;
    int32_t win_x1;
    int32_t win_y1;
    int32_t win_x2;
    int32_t win_y2;
    int32_t width_mm;
    int32_t height_mm;
};

/* The 4bpp EBC framebuffer region size from the header (EBC_FB_SIZE 0x200000). We mmap a
 * generous span and clamp; the driver ignores vm_pgoff and maps from base anyway. */
#define EBC_FB_SIZE_GUESS (0x400000) /* 4 MiB — covers EBC_FB_SIZE/EINK_FB_SIZE */

/* State held across one R3 probe (single-threaded, UI thread). */
static int      g_fd = -1;
static uint8_t *g_map = NULL;
static size_t   g_map_len = 0;

/*
 * Append one line to a Kotlin StringBuilder-style report. We instead return a single Java
 * String built here so the Activity can show it; keep messages short and factual.
 */

/* Route 3 probe: open, GET_BUFFER_INFO, mmap, then one A2 partial update of [x1,y1,x2,y2].
 * Returns a human-readable multi-line report string. Every rc/errno is reported — a failure
 * (e.g. EACCES under untrusted_app SELinux) is a RESULT, not an error to hide. */
JNIEXPORT jstring JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_probeA2(
        JNIEnv *env, jclass clazz,
        jint x1, jint y1, jint x2, jint y2) {
    (void) clazz;
    char buf[1024];
    int n = 0;
#define APPEND(...) do { n += snprintf(buf + n, sizeof(buf) - (size_t)n, __VA_ARGS__); } while (0)

    /* 1) open(/dev/ebc) — THE SELinux make-or-break for untrusted_app. */
    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        int e = errno;
        LOGE("open(/dev/ebc) FAILED errno=%d (%s)", e, strerror(e));
        APPEND("open(/dev/ebc)=FAILED errno=%d(%s)", e, strerror(e));
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("open(/dev/ebc)=OK fd=%d", fd);
    APPEND("open(/dev/ebc)=OK fd=%d; ", fd);

    /* 2) GET_BUFFER_INFO — panel geometry. */
    struct ebc_buf_info info;
    memset(&info, 0, sizeof(info));
    if (ioctl(fd, EBC_GET_BUFFER_INFO, &info) < 0) {
        int e = errno;
        LOGE("ioctl(GET_BUFFER_INFO) FAILED errno=%d (%s)", e, strerror(e));
        APPEND("GET_BUFFER_INFO=FAILED errno=%d(%s)", e, strerror(e));
        close(fd);
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("GET_BUFFER_INFO=OK w=%d h=%d color=%d", info.width, info.height, info.panel_color);
    APPEND("GET_BUFFER_INFO=OK w=%d h=%d color=%d; ", info.width, info.height, info.panel_color);

    /* 3) mmap the framebuffer region (from offset 0; driver ignores vm_pgoff). */
    size_t map_len = EBC_FB_SIZE_GUESS;
    uint8_t *map = (uint8_t *) mmap(NULL, map_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) {
        int e = errno;
        LOGE("mmap FAILED errno=%d (%s)", e, strerror(e));
        APPEND("mmap=FAILED errno=%d(%s)", e, strerror(e));
        close(fd);
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("mmap=OK len=%zu", map_len);
    APPEND("mmap=OK; ");

    /* 4) GET_BUFFER — grab a free framebuffer, get its offset. */
    struct ebc_buf_info draw = info;
    if (ioctl(fd, EBC_GET_BUFFER, &draw) < 0) {
        int e = errno;
        LOGE("ioctl(GET_BUFFER) FAILED errno=%d (%s)", e, strerror(e));
        APPEND("GET_BUFFER=FAILED errno=%d(%s)", e, strerror(e));
        munmap(map, map_len);
        close(fd);
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("GET_BUFFER=OK offset=%d", draw.offset);
    APPEND("GET_BUFFER=OK off=%d; ", draw.offset);

    /* 5) Paint the bbox black (4bpp packed grayscale: 0x00 = darkest) into base+offset.
     *    We only touch the dirty rect's rows to keep it cheap and visible. Stride = vir
     *    width; we do not know vir_width for certain, so use width (HIGH risk of stride
     *    mismatch — flagged). This is a *visibility* probe, not pixel-perfect ink. */
    if (draw.offset >= 0 && (size_t) draw.offset < map_len) {
        uint8_t *fb = map + draw.offset;
        int W = info.width > 0 ? info.width : 1872;
        int H = info.height > 0 ? info.height : 1404;
        int cx1 = x1 < 0 ? 0 : (x1 >= W ? W - 1 : x1);
        int cy1 = y1 < 0 ? 0 : (y1 >= H ? H - 1 : y1);
        int cx2 = x2 <= cx1 ? cx1 + 1 : (x2 > W ? W : x2);
        int cy2 = y2 <= cy1 ? cy1 + 1 : (y2 > H ? H : y2);
        /* 4bpp => 2 px/byte; row stride in bytes = W/2. */
        size_t stride = (size_t) (W / 2);
        for (int y = cy1; y < cy2; ++y) {
            size_t row = (size_t) y * stride;
            for (int x = cx1; x < cx2; ++x) {
                size_t bi = row + (size_t) (x / 2);
                if (bi < map_len - (size_t) draw.offset) {
                    fb[bi] = 0x00; /* both nibbles dark */
                }
            }
        }
        LOGI("painted bbox [%d,%d,%d,%d] stride=%zu", cx1, cy1, cx2, cy2, stride);
    }

    /* 6) SEND_BUFFER with EPD_A2 + the dirty rect. */
    draw.epd_mode = EPD_A2;
    draw.win_x1 = x1; draw.win_y1 = y1; draw.win_x2 = x2; draw.win_y2 = y2;
    if (ioctl(fd, EBC_SEND_BUFFER, &draw) < 0) {
        int e = errno;
        LOGE("ioctl(SEND_BUFFER A2) FAILED errno=%d (%s)", e, strerror(e));
        APPEND("SEND_BUFFER(A2)=FAILED errno=%d(%s)", e, strerror(e));
    } else {
        LOGI("SEND_BUFFER(A2)=OK mode=%d rect=[%d,%d,%d,%d]", EPD_A2, x1, y1, x2, y2);
        APPEND("SEND_BUFFER(A2)=OK mode=12 rect=[%d,%d,%d,%d]", x1, y1, x2, y2);
    }

    munmap(map, map_len);
    close(fd);
    return (*env)->NewStringUTF(env, buf);
#undef APPEND
}

/* Lightweight reachability-only check: just open()+close(), report errno. Used on every R3
 * stroke to keep a latency loop cheap (full probeA2 is the one-shot diagnostic). */
JNIEXPORT jint JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_canOpen(JNIEnv *env, jclass clazz) {
    (void) env; (void) clazz;
    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        int e = errno;
        LOGE("canOpen: open(/dev/ebc) errno=%d (%s)", e, strerror(e));
        return -e; /* negative errno */
    }
    close(fd);
    return 0;
}

/* Persistent-session API for the per-stroke A2 path (Route 3 latency loop):
 * openEbc() once, then sendA2(rect) per stroke, then closeEbc(). Keeps fd+mmap open so each
 * stroke is just GET_BUFFER/paint/SEND_BUFFER. Returns 0 on success, negative errno on fail. */
JNIEXPORT jint JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_openEbc(JNIEnv *env, jclass clazz) {
    (void) env; (void) clazz;
    if (g_fd >= 0) return 0;
    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) return -errno;
    struct ebc_buf_info info;
    memset(&info, 0, sizeof(info));
    if (ioctl(fd, EBC_GET_BUFFER_INFO, &info) < 0) { int e = errno; close(fd); return -e; }
    size_t len = EBC_FB_SIZE_GUESS;
    uint8_t *map = (uint8_t *) mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) { int e = errno; close(fd); return -e; }
    g_fd = fd; g_map = map; g_map_len = len;
    LOGI("openEbc OK fd=%d w=%d h=%d", fd, info.width, info.height);
    return 0;
}

JNIEXPORT jint JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_sendA2(
        JNIEnv *env, jclass clazz, jint x1, jint y1, jint x2, jint y2) {
    (void) env; (void) clazz;
    if (g_fd < 0) return -EBADF;
    struct ebc_buf_info d;
    memset(&d, 0, sizeof(d));
    if (ioctl(g_fd, EBC_GET_BUFFER, &d) < 0) return -errno;
    d.epd_mode = EPD_A2;
    d.win_x1 = x1; d.win_y1 = y1; d.win_x2 = x2; d.win_y2 = y2;
    if (ioctl(g_fd, EBC_SEND_BUFFER, &d) < 0) return -errno;
    return 0;
}

JNIEXPORT void JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_closeEbc(JNIEnv *env, jclass clazz) {
    (void) env; (void) clazz;
    if (g_map && g_map != MAP_FAILED) munmap(g_map, g_map_len);
    if (g_fd >= 0) close(g_fd);
    g_map = NULL; g_map_len = 0; g_fd = -1;
}
