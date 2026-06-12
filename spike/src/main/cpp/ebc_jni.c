/*
 * ebc_jni.c — native /dev/ebc probe for the RR19-FR4b pen-latency spike (Route 3).
 *
 * CLEAN-ROOM (RR18). The EBC ioctl ABI below is reimplemented SOLELY from the public,
 * GPL Rockchip `ebc-dev` kernel UAPI header — a documented contract — NOT from any
 * decompiled Ratta/Onyx code.
 *
 * ON-DEVICE FINDINGS (real rk3566_ht_eink, untrusted_app domain):
 *   - open("/dev/ebc", O_RDWR) = OK under untrusted_app SELinux (the make-or-break — NOT EACCES).
 *   - ioctl(0x7002, &buf44) → EINVAL(22), NOT ENOTTY(25): the cmd is RECOGNIZED, the ARG was
 *     wrong. The device's `uname -r` = 4.19.193 → it ships the develop-4.19 ebc-dev driver,
 *     whose `struct ebc_buf_info` is **64 bytes** (adds `int needpic` + `char tid_name[16]`),
 *     not the 44-byte develop-5.10 layout the original probe used. The 20-byte size mismatch is
 *     the EINVAL cause (the driver copy_from_user's its own sizeof). So this revision makes the
 *     **64-byte 13-field struct PRIMARY** and the discovery probe stays empirical (44/48/64/256).
 *
 * Sources (verbatim ioctl #defines, struct, enum, FB sizes taken from):
 *   Rockchip BSP develop-4.19 (THIS device's kernel lineage — confirmed `uname -r` 4.19.193):
 *     https://raw.githubusercontent.com/rockchip-linux/kernel/develop-4.19/\
 *       drivers/gpu/drm/rockchip/ebc-dev/ebc_dev.h
 *   Rockchip BSP develop-5.10 (the 44-byte variant, for the size matrix):
 *     github.com/rockchip-linux/kernel (develop-5.10) .../ebc-dev/ebc_dev.h
 *   smaeul/linux (mainline DRM port; enum >=13 diverges — only used to bound the matrix):
 *     github.com/smaeul/linux .../ebc-dev/ebc_dev.h
 *   Buffer/mmap-offset model (clean-room RE, read for *flow* only — NOT copied):
 *     github.com/Ralim/ebc-dev-reverse-engineering done/ebc_dev_v8.c
 *
 * ABI facts (HIGH confidence — primary-source fetch of develop-4.19 header):
 *  - RAW integer ioctl request codes 0x7000..0x700d (NOT _IO* macros; no magic/size/dir).
 *  - 4.19 `struct ebc_buf_info` = 12 ints (48B) + char[16] = **64 bytes**.
 *  - enum: EPD_PART_GC16=7, EPD_A2=12, EPD_DU=14 (4.19).
 *  - mmap maps the whole FB CMA region from offset 0; `offset` is a byte offset into it.
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

/* ---- Rockchip ebc-dev UAPI (clean-room; develop-4.19 header — see banner) ---- */

/* Raw integer ioctl request codes — develop-4.19 set (0x7000..0x700d). */
#define EBC_GET_BUFFER         (0x7000)
#define EBC_SEND_BUFFER        (0x7001)
#define EBC_GET_BUFFER_INFO    (0x7002)
#define EBC_SET_FULL_MODE_NUM  (0x7003)
#define EBC_ENABLE_OVERLAY     (0x7004)
#define EBC_DISABLE_OVERLAY    (0x7005)
#define EBC_GET_OSD_BUFFER     (0x7006)
#define EBC_SEND_OSD_BUFFER    (0x7007)
#define EBC_NEW_BUF_PREPARE    (0x7008)
#define EBC_SET_DIFF_PERCENT   (0x7009)
#define EBC_WAIT_NEW_BUF_TIME  (0x700a)
#define EBC_GET_OVERLAY_STATUS (0x700b)
#define EBC_ENABLE_BG_CONTROL  (0x700c)
#define EBC_DISABLE_BG_CONTROL (0x700d)

/* enum panel_refresh_mode (develop-4.19). */
enum panel_refresh_mode {
    EPD_AUTO       = 0,
    EPD_OVERLAY    = 1,
    EPD_FULL_GC16  = 2,
    EPD_PART_GC16  = 7,
    EPD_A2         = 12, /* fast/handwriting waveform */
    EPD_DU         = 14, /* 4.19: A2_DITHER=13, DU=14 */
};

/* struct ebc_buf_info — develop-4.19: 64 bytes (12 ints + char[16]). PRIMARY layout. */
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
    int32_t needpic;        /* +1 int vs 5.10 */
    char    tid_name[16];   /* +16 bytes vs 5.10  => total 64 */
};

/* ABI guard: the develop-4.19 ebc_buf_info MUST be 64 bytes (the EINVAL fix). If a field
 * edit ever changes this, fail the build rather than ship a silent size regression. */
_Static_assert(sizeof(struct ebc_buf_info) == 64, "ebc_buf_info must be 64 bytes (rk develop-4.19)");

/* FB region sizes (develop-4.19 header): EBC_FB_SIZE 2M, EINK_FB_SIZE 4M. */
#define EBC_FB_SIZE_GUESS (0x400000) /* 4 MiB — covers EBC_FB_SIZE/EINK_FB_SIZE */

/* Persistent session for the per-stroke A2 latency loop. */
static int      g_fd = -1;
static uint8_t *g_map = NULL;
static size_t   g_map_len = 0;

/* errno number -> short name, for the discovery table's readability. */
static const char *errno_name(int e) {
    switch (e) {
        case 0:        return "OK";
        case EPERM:    return "EPERM";
        case EBADF:    return "EBADF";
        case EFAULT:   return "EFAULT";   /* recognized, bad pointer */
        case EINVAL:   return "EINVAL";   /* recognized, bad arg (size/content/state) */
        case ENOTTY:   return "ENOTTY";   /* UNRECOGNIZED cmd */
        case EACCES:   return "EACCES";
        case ENODEV:   return "ENODEV";
        case ENOSYS:   return "ENOSYS";
        case EAGAIN:   return "EAGAIN";
        case ENOMEM:   return "ENOMEM";
        case ENOTSUP:  return "ENOTSUP";
        default:       return "?";
    }
}

/* _IO* macro encodings, for the discovery matrix (some forks COULD be macro-encoded). The
 * Linux ioctl number layout: dir(2b)<<30 | size(14b)<<16 | type(8b)<<8 | nr(8b). */
#define IOC_NONE_  0u
#define IOC_WRITE_ 1u
#define IOC_READ_  2u
#define ENC_IO(dir, type, nr, size) \
    ( ((dir) << 30) | (((unsigned)(size) & 0x3FFFu) << 16) | (((unsigned)(type) & 0xFFu) << 8) | ((unsigned)(nr) & 0xFFu) )

/* ============================ DISCOVERY PROBE ============================ */
/*
 * EbcNative.discoverAbi(): with /dev/ebc open, run a CURATED clean-room candidate matrix and
 * return a human-readable table. For each (cmd, argsize): ioctl(fd, cmd, buf) with buf = a
 * zeroed 4096-byte buffer (valid pointer), then classify the result:
 *   rc==0            => OK (cmd recognized, succeeded)
 *   EINVAL(22)       => recognized, wrong ARG (size/content/state)  <-- likely the real cmd
 *   EFAULT(14)       => recognized, bad pointer
 *   ENOTTY(25)       => UNRECOGNIZED cmd
 * The non-ENOTTY rows are the discovery. On a GET_BUFFER_INFO success we dump the first 16
 * ints so width/height/offset/epd_mode can be read back and the layout confirmed.
 *
 * Candidate cmd set (clean-room; sources cited in the file banner):
 *   - raw 0x7000..0x700d : develop-4.19 set (PRIMARY) and develop-5.10 subset 0x7000..0x7007.
 *   - _IO* encodings with magic 'E'(0x45) and 'F'(0x46), nr 0x00..0x07, sizes 44/48/64 :
 *     a defensive hedge in case some downstream fork macro-encoded the codes. (No public RK
 *     ebc-dev fork is known to use _IO* — they are all raw ints — so these are expected to
 *     ENOTTY; included only to PROVE that empirically on THIS device.)
 *   - the classic TTY 0x5401 (TCGETS) as a CONTROL: a char device that is NOT a tty returns
 *     ENOTTY/EINVAL here, confirming our ENOTTY classifier actually fires.
 */
JNIEXPORT jstring JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_discoverAbi(JNIEnv *env, jclass clazz) {
    (void) clazz;

    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        char e[160];
        snprintf(e, sizeof(e), "discoverAbi: open(/dev/ebc) FAILED errno=%d(%s)\n",
                 errno, errno_name(errno));
        LOGE("%s", e);
        return (*env)->NewStringUTF(env, e);
    }

    /* Big report buffer; we append rows. */
    static char rpt[8192];
    int n = 0;
#define ROW(...) do { n += snprintf(rpt + n, sizeof(rpt) - (size_t)n, __VA_ARGS__); } while (0)

    ROW("=== /dev/ebc ioctl ABI discovery (fd=%d) ===\n", fd);
    ROW("legend: ENOTTY=unrecognized  EINVAL=recognized-bad-arg  EFAULT=recognized-bad-ptr  OK=success\n");

    /* A valid, zeroed argument buffer (4096B) so a bad pointer is never the cause. */
    static uint8_t argbuf[4096];

    /* ---- Phase 1: raw-int candidates 0x7000..0x700d, default 64B arg already covered by argbuf. ---- */
    ROW("-- phase 1: raw-int cmds 0x7000..0x700d (arg=zeroed 4096B) --\n");
    static const struct { unsigned cmd; const char *name; } raw[] = {
        { EBC_GET_BUFFER,         "GET_BUFFER" },
        { EBC_SEND_BUFFER,        "SEND_BUFFER" },
        { EBC_GET_BUFFER_INFO,    "GET_BUFFER_INFO" },
        { EBC_SET_FULL_MODE_NUM,  "SET_FULL_MODE_NUM" },
        { EBC_ENABLE_OVERLAY,     "ENABLE_OVERLAY" },
        { EBC_DISABLE_OVERLAY,    "DISABLE_OVERLAY" },
        { EBC_GET_OSD_BUFFER,     "GET_OSD_BUFFER" },
        { EBC_SEND_OSD_BUFFER,    "SEND_OSD_BUFFER" },
        { EBC_NEW_BUF_PREPARE,    "NEW_BUF_PREPARE" },
        { EBC_SET_DIFF_PERCENT,   "SET_DIFF_PERCENT" },
        { EBC_WAIT_NEW_BUF_TIME,  "WAIT_NEW_BUF_TIME" },
        { EBC_GET_OVERLAY_STATUS, "GET_OVERLAY_STATUS" },
        { EBC_ENABLE_BG_CONTROL,  "ENABLE_BG_CONTROL" },
        { EBC_DISABLE_BG_CONTROL, "DISABLE_BG_CONTROL" },
    };
    for (size_t i = 0; i < sizeof(raw) / sizeof(raw[0]); ++i) {
        memset(argbuf, 0, sizeof(argbuf));
        int rc = ioctl(fd, raw[i].cmd, argbuf);
        int e = (rc < 0) ? errno : 0;
        ROW("  cmd=0x%04x %-20s rc=%d errno=%d(%s)\n",
            raw[i].cmd, raw[i].name, rc, e, errno_name(e));
        LOGI("disco raw cmd=0x%04x %s rc=%d errno=%d(%s)", raw[i].cmd, raw[i].name, rc, e, errno_name(e));
    }

    /* ---- Phase 2: GET_BUFFER_INFO (0x7002) across struct sizes 40/44/48/64/256. ---- */
    ROW("-- phase 2: GET_BUFFER_INFO=0x7002 across arg sizes (size only affects what the\n");
    ROW("   driver copy_from_user reads; we pass a valid 4096B buf regardless) --\n");
    static const int sizes[] = { 40, 44, 48, 64, 256 };
    for (size_t i = 0; i < sizeof(sizes) / sizeof(sizes[0]); ++i) {
        /* Note: the cmd encodes nothing about size for raw ints — the DRIVER uses its own
         * sizeof. So varying the *passed* size cannot change a raw-int result; we still log
         * it to make that explicit and to exercise any macro-encoded GET_BUFFER_INFO below. */
        memset(argbuf, 0, sizeof(argbuf));
        int rc = ioctl(fd, EBC_GET_BUFFER_INFO, argbuf);
        int e = (rc < 0) ? errno : 0;
        ROW("  GET_BUFFER_INFO(raw 0x7002) declaredArgSize=%-3d rc=%d errno=%d(%s)\n",
            sizes[i], rc, e, errno_name(e));
    }

    /* ---- Phase 3: _IO* macro-encoded candidates (magic 'E'/'F', nr 0x00..0x07, sizes 44/48/64). ---- */
    ROW("-- phase 3: _IOWR macro encodings magic 'E'(0x45)/'F'(0x46) (hedge; expect ENOTTY) --\n");
    static const int macro_sizes[] = { 44, 48, 64 };
    static const unsigned char magics[] = { 'E', 'F' };
    for (size_t m = 0; m < sizeof(magics); ++m) {
        for (unsigned nr = 0; nr <= 0x07; ++nr) {
            for (size_t s = 0; s < sizeof(macro_sizes) / sizeof(macro_sizes[0]); ++s) {
                unsigned cmd = ENC_IO(IOC_READ_ | IOC_WRITE_, magics[m], nr, macro_sizes[s]);
                memset(argbuf, 0, sizeof(argbuf));
                int rc = ioctl(fd, cmd, argbuf);
                int e = (rc < 0) ? errno : 0;
                /* Only log the INTERESTING ones (non-ENOTTY) to keep the table readable. */
                if (e != ENOTTY) {
                    ROW("  _IOWR('%c',0x%02x,sz%d)=0x%08x rc=%d errno=%d(%s)  <-- recognized!\n",
                        magics[m], nr, macro_sizes[s], cmd, rc, e, errno_name(e));
                    LOGI("disco macro _IOWR('%c',0x%02x,%d)=0x%08x rc=%d errno=%d(%s)",
                         magics[m], nr, macro_sizes[s], cmd, rc, e, errno_name(e));
                }
            }
        }
    }
    ROW("  (phase 3: only non-ENOTTY rows shown; if none, no macro-encoded cmd was recognized)\n");

    /* ---- Control: TCGETS 0x5401 on a non-tty should be ENOTTY/EINVAL (sanity for our classifier). ---- */
    memset(argbuf, 0, sizeof(argbuf));
    int crc = ioctl(fd, 0x5401u /*TCGETS*/, argbuf);
    int ce = (crc < 0) ? errno : 0;
    ROW("-- control: TCGETS(0x5401) rc=%d errno=%d(%s) (non-tty => expect ENOTTY/EINVAL) --\n",
        crc, ce, errno_name(ce));

    /* ---- Phase 4: if raw GET_BUFFER_INFO succeeded with the 64B struct, dump the ints. ---- */
    {
        struct ebc_buf_info info;
        memset(&info, 0, sizeof(info));
        int rc = ioctl(fd, EBC_GET_BUFFER_INFO, &info);
        int e = (rc < 0) ? errno : 0;
        ROW("-- phase 4: GET_BUFFER_INFO with the 64B struct rc=%d errno=%d(%s) (sizeof=%zu) --\n",
            rc, e, errno_name(e), sizeof(info));
        if (rc == 0) {
            const int32_t *p = (const int32_t *) &info;
            ROW("   first ints: ");
            for (int k = 0; k < 12; ++k) ROW("%d ", p[k]);
            ROW("\n   => offset=%d epd_mode=%d height=%d width=%d panel_color=%d win[%d,%d,%d,%d] mm[%d,%d] needpic=%d\n",
                info.offset, info.epd_mode, info.height, info.width, info.panel_color,
                info.win_x1, info.win_y1, info.win_x2, info.win_y2,
                info.width_mm, info.height_mm, info.needpic);
            /* Try the full flow: GET_BUFFER + mmap, to confirm the buffer model. */
            struct ebc_buf_info draw;
            memset(&draw, 0, sizeof(draw));
            int grc = ioctl(fd, EBC_GET_BUFFER, &draw);
            ROW("   GET_BUFFER rc=%d errno=%d(%s) offset=%d\n",
                grc, (grc < 0 ? errno : 0), errno_name(grc < 0 ? errno : 0), draw.offset);
            uint8_t *map = (uint8_t *) mmap(NULL, EBC_FB_SIZE_GUESS, PROT_READ | PROT_WRITE,
                                            MAP_SHARED, fd, 0);
            if (map == MAP_FAILED) {
                ROW("   mmap FAILED errno=%d(%s)\n", errno, errno_name(errno));
            } else {
                ROW("   mmap OK (%d bytes)\n", EBC_FB_SIZE_GUESS);
                munmap(map, EBC_FB_SIZE_GUESS);
            }
        }
    }

    /* ---- Phase 5: the device's REAL driver is Ratta ht_eink (ht_ebc.px), NOT the stock
       rockchip ebc-dev. It uses a private 'HT' (0x4854_xxxx, ASCII "HT") ioctl family with a
       fixed 2132-byte arg struct; the handler returns -EINVAL for every unknown cmd (which is
       why all of phases 1-4 above were uniformly EINVAL). Numbers derived clean-room from the
       compositor's ioctl call sites + the GPL ht_ebc.px symbol table (no decompiled bodies).
       0x48545201 ('R'/1) is the GETINFO probe the compositor issues right after open(). ---- */
    {
        /* SAFETY: probe ONLY the read-only GETINFO cmd. The write/refresh/setbuf 'HT' cmds
           (W/2, X/1, B/n) are real panel operations — firing them blind with a garbage 2132B
           arg rebooted the device once. Do NOT add them here without a known, validated arg
           layout. GETINFO ('R'/1) is the compositor's probe-after-open; it only reads geometry. */
        ROW("-- phase 5: Ratta 'HT' GETINFO probe 0x48545201 (READ-ONLY; arg=2132B zeroed) --\n");
        memset(argbuf, 0, sizeof(argbuf)); /* 4096B static buf covers the 2132B arg */
        int rc = ioctl(fd, 0x48545201u, argbuf);
        int e = (rc < 0) ? errno : 0;
        ROW("  cmd=0x48545201 HT 'R'/1 GETINFO  rc=%d errno=%d(%s)\n", rc, e, errno_name(e));
        if (rc == 0) {
            const int32_t *p = (const int32_t *) argbuf;
            ROW("   GETINFO ok — first 16 ints: ");
            for (int k = 0; k < 16; ++k) ROW("%d ", p[k]);
            ROW("\n");
        }
    }

    ROW("=== end discovery ===\n");
    close(fd);
    LOGI("%s", rpt);
    return (*env)->NewStringUTF(env, rpt);
#undef ROW
}

/* ============================ ROUTE-3 PROBE (64B struct) ============================ */
/* One-shot: open → GET_BUFFER_INFO(64B) → mmap → GET_BUFFER → paint bbox → SEND_BUFFER(A2). */
JNIEXPORT jstring JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_probeA2(
        JNIEnv *env, jclass clazz,
        jint x1, jint y1, jint x2, jint y2) {
    (void) clazz;
    char buf[1024];
    int n = 0;
#define APPEND(...) do { n += snprintf(buf + n, sizeof(buf) - (size_t)n, __VA_ARGS__); } while (0)

    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        int e = errno;
        LOGE("open(/dev/ebc) FAILED errno=%d (%s)", e, errno_name(e));
        APPEND("open(/dev/ebc)=FAILED errno=%d(%s)", e, errno_name(e));
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("open(/dev/ebc)=OK fd=%d", fd);
    APPEND("open(/dev/ebc)=OK fd=%d; ", fd);

    struct ebc_buf_info info;
    memset(&info, 0, sizeof(info));
    if (ioctl(fd, EBC_GET_BUFFER_INFO, &info) < 0) {
        int e = errno;
        LOGE("ioctl(GET_BUFFER_INFO,64B) FAILED errno=%d (%s)", e, errno_name(e));
        APPEND("GET_BUFFER_INFO(64B)=FAILED errno=%d(%s); run discoverAbi()", e, errno_name(e));
        close(fd);
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("GET_BUFFER_INFO=OK w=%d h=%d color=%d", info.width, info.height, info.panel_color);
    APPEND("GET_BUFFER_INFO(64B)=OK w=%d h=%d color=%d; ", info.width, info.height, info.panel_color);

    size_t map_len = EBC_FB_SIZE_GUESS;
    uint8_t *map = (uint8_t *) mmap(NULL, map_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) {
        int e = errno;
        LOGE("mmap FAILED errno=%d (%s)", e, errno_name(e));
        APPEND("mmap=FAILED errno=%d(%s)", e, errno_name(e));
        close(fd);
        return (*env)->NewStringUTF(env, buf);
    }
    APPEND("mmap=OK; ");

    struct ebc_buf_info draw = info;
    if (ioctl(fd, EBC_GET_BUFFER, &draw) < 0) {
        int e = errno;
        LOGE("ioctl(GET_BUFFER) FAILED errno=%d (%s)", e, errno_name(e));
        APPEND("GET_BUFFER=FAILED errno=%d(%s)", e, errno_name(e));
        munmap(map, map_len);
        close(fd);
        return (*env)->NewStringUTF(env, buf);
    }
    LOGI("GET_BUFFER=OK offset=%d", draw.offset);
    APPEND("GET_BUFFER=OK off=%d; ", draw.offset);

    /* Paint the bbox dark (4bpp packed grayscale, 2px/byte, stride=W/2). Visibility probe;
     * stride/bpp may differ (flagged) — the ioctl rc is the reliable reachability signal. */
    if (draw.offset >= 0 && (size_t) draw.offset < map_len) {
        uint8_t *fb = map + draw.offset;
        int W = info.width > 0 ? info.width : 1872;
        int H = info.height > 0 ? info.height : 1404;
        int cx1 = x1 < 0 ? 0 : (x1 >= W ? W - 1 : x1);
        int cy1 = y1 < 0 ? 0 : (y1 >= H ? H - 1 : y1);
        int cx2 = x2 <= cx1 ? cx1 + 1 : (x2 > W ? W : x2);
        int cy2 = y2 <= cy1 ? cy1 + 1 : (y2 > H ? H : y2);
        size_t stride = (size_t) (W / 2);
        for (int y = cy1; y < cy2; ++y) {
            size_t row = (size_t) y * stride;
            for (int x = cx1; x < cx2; ++x) {
                size_t bi = row + (size_t) (x / 2);
                if (bi < map_len - (size_t) draw.offset) fb[bi] = 0x00;
            }
        }
    }

    draw.epd_mode = EPD_A2;
    draw.win_x1 = x1; draw.win_y1 = y1; draw.win_x2 = x2; draw.win_y2 = y2;
    if (ioctl(fd, EBC_SEND_BUFFER, &draw) < 0) {
        int e = errno;
        LOGE("ioctl(SEND_BUFFER A2) FAILED errno=%d (%s)", e, errno_name(e));
        APPEND("SEND_BUFFER(A2)=FAILED errno=%d(%s)", e, errno_name(e));
    } else {
        LOGI("SEND_BUFFER(A2)=OK mode=%d rect=[%d,%d,%d,%d]", EPD_A2, x1, y1, x2, y2);
        APPEND("SEND_BUFFER(A2)=OK mode=12 rect=[%d,%d,%d,%d]", x1, y1, x2, y2);
    }

    munmap(map, map_len);
    close(fd);
    return (*env)->NewStringUTF(env, buf);
#undef APPEND
}

/* ============================ HT GETINFO (READ-ONLY) ============================ */
/*
 * EbcNative.htGetInfo(): the decisive, SAFE direct-path probe for THIS device's real driver.
 *
 * The panel is driven by Ratta `ht_eink` (ht_ebc.px), a private 'HT' (0x4854xxxx) ioctl family
 * — NOT stock rockchip ebc-dev (so probeA2/sendA2's 0x70xx codes return EINVAL here, see banner
 * phase-5 note). 0x48545201 ('HT','R'/1) is the GETINFO the compositor issues right after open();
 * it ONLY reads geometry. This is the ONE direct-path call known to be safe to fire (the 'HT'
 * WRITE/REFRESH cmds reboot the device when fired blind — DO NOT add them here without a
 * validated arg layout).
 *
 * Verdict semantics (single greppable line, prefix "HT-GETINFO:"):
 *   rc=0           => two-way comms with the real driver CONFIRMED; geometry dumped → the
 *                     foundation a future NON-blind HT write needs. Direct path is alive.
 *   EINVAL(22)     => 'HT' family present but THIS cmd/arg-size is wrong → direct path needs
 *                     more recon than we have; tilt to DrawService / privileged install.
 *   ENOTTY(25)     => not even the 'HT' family → driver assumption wrong.
 *   open EACCES    => SELinux closed the door for untrusted_app → sideload direct path is dead.
 *
 * The 'HT' arg is a fixed 2132-byte struct; we pass a zeroed 4096B buffer (a valid, oversized
 * pointer) so a bad pointer is never the cause, and surface candidate panel dims (1920x2560 /
 * 1872x1404) found among the first ints so the layout can be pinned without a blind write.
 */
JNIEXPORT jstring JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_htGetInfo(JNIEnv *env, jclass clazz) {
    (void) clazz;
    static char rpt[2048];
    int n = 0;
#define ROW(...) do { n += snprintf(rpt + n, sizeof(rpt) - (size_t)n, __VA_ARGS__); } while (0)

    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        int e = errno;
        ROW("HT-GETINFO: open(/dev/ebc)=FAILED errno=%d(%s) => sideload direct path DEAD\n",
            e, errno_name(e));
        LOGE("%s", rpt);
        return (*env)->NewStringUTF(env, rpt);
    }

    /* 2132-byte 'HT' arg; oversized zeroed buffer so the pointer is always valid. READ-ONLY cmd. */
    static uint8_t arg[4096];
    memset(arg, 0, sizeof(arg));
    int rc = ioctl(fd, 0x48545201u /* 'HT','R'/1 GETINFO */, arg);
    int e = (rc < 0) ? errno : 0;

    if (rc != 0) {
        ROW("HT-GETINFO: cmd=0x48545201 rc=%d errno=%d(%s) => direct path NOT confirmed "
            "(EINVAL=arg/cmd wrong; pivot to DrawService/privileged install)\n",
            rc, e, errno_name(e));
        LOGI("%s", rpt);
        close(fd);
        return (*env)->NewStringUTF(env, rpt);
    }

    /* rc==0: real two-way comms. Surface the geometry the (future, non-blind) write needs. */
    ROW("HT-GETINFO: cmd=0x48545201 rc=0 OK => two-way comms with ht_eink CONFIRMED (direct path ALIVE)\n");
    const int32_t *p = (const int32_t *) arg;
    ROW("  first 32 ints:");
    for (int k = 0; k < 32; ++k) ROW(" %d", p[k]);
    ROW("\n");
    /* Flag any int pair that matches a known Supernote panel geometry (clean-room: 1920x2560
     * per spec; 1872x1404 is the other common rk eink size) — pins width/height offsets. */
    for (int k = 0; k < 32; ++k) {
        int v = p[k];
        if (v == 1920 || v == 2560 || v == 1872 || v == 1404) {
            ROW("  candidate geometry: ints[%d]=%d\n", k, v);
        }
    }

    /* Full READ-ONLY dump of the 2132-byte 'HT' GETINFO reply to logcat (PenSpike-ebc), so it
     * can be mapped field-by-field against the private recon header — the raw material for
     * building a STRUCTURED (non-blind) write arg later. Fires no write/refresh cmd. */
    enum { HT_ARG_BYTES = 2132 };
    LOGI("HT-GETINFO full dump (%d bytes, read-only):", HT_ARG_BYTES);
    {
        char line[80];
        for (int off = 0; off < HT_ARG_BYTES; off += 16) {
            int m = 0;
            m += snprintf(line + m, sizeof(line) - (size_t) m, "  +%04x:", off);
            for (int j = 0; j < 16 && off + j < HT_ARG_BYTES; ++j)
                m += snprintf(line + m, sizeof(line) - (size_t) m, " %02x", arg[off + j]);
            LOGI("%s", line);
        }
    }

    /* READ-ONLY framebuffer reachability: can we mmap the EBC FB region? Prerequisite for the
     * write path's draw buffer. PROT_READ only — mapping + reading never drives the panel. */
    {
        size_t len = EBC_FB_SIZE_GUESS;
        uint8_t *map = (uint8_t *) mmap(NULL, len, PROT_READ, MAP_SHARED, fd, 0);
        if (map == MAP_FAILED) {
            LOGI("HT-GETINFO mmap(PROT_READ) FAILED errno=%d(%s)", errno, errno_name(errno));
            ROW("  mmap(read)=FAILED errno=%d(%s)\n", errno, errno_name(errno));
        } else {
            LOGI("HT-GETINFO mmap(PROT_READ,%zuB)=OK first8=%02x %02x %02x %02x %02x %02x %02x %02x",
                 len, map[0], map[1], map[2], map[3], map[4], map[5], map[6], map[7]);
            ROW("  mmap(read)=OK %zuB\n", len);
            munmap(map, len);
        }
    }

    LOGI("%s", rpt);
    close(fd);
    return (*env)->NewStringUTF(env, rpt);
#undef ROW
}

/* ============================ HT FB READBACK (READ-ONLY) ============================ */
/*
 * EbcNative.htDumpFb(): mmap the EBC framebuffer PROT_READ and render a small ASCII thumbnail
 * of what's currently in it (4bpp packed grayscale, white=0xF, stride=W/2). The decisive,
 * zero-risk question: is mmap offset 0 the LIVE panel buffer? If the thumbnail matches the
 * on-screen content (the spike legend band at the top), then offset-0 = live buffer is proven
 * → software readback works (camera-free latency/verification) and the write path is simply
 * "paint into this FB + HT refresh". Read-only — issues NO write/refresh cmd (those reboot).
 *
 * Geometry comes from the safe HT GETINFO (0x48545201): width @ +0x04(u16), height @ +0x06(u16).
 */
JNIEXPORT jstring JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_htDumpFb(JNIEnv *env, jclass clazz) {
    (void) clazz;
    static char out[256];

    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        snprintf(out, sizeof(out), "HT-FB: open FAILED errno=%d(%s)", errno, errno_name(errno));
        return (*env)->NewStringUTF(env, out);
    }

    /* Geometry from GETINFO (width/height as u16 at +0x04/+0x06; see htinfo dump). */
    static uint8_t info[4096];
    memset(info, 0, sizeof(info));
    int W = 1920, H = 2560;
    if (ioctl(fd, 0x48545201u, info) == 0) {
        int w = (int) (info[4] | (info[5] << 8));
        int h = (int) (info[6] | (info[7] << 8));
        if (w >= 320 && w <= 4096) W = w;
        if (h >= 320 && h <= 4096) H = h;
    }
    size_t stride = (size_t) (W / 2); /* 4bpp packed: 2 px/byte */

    uint8_t *map = (uint8_t *) mmap(NULL, EBC_FB_SIZE_GUESS, PROT_READ, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) {
        snprintf(out, sizeof(out), "HT-FB: mmap FAILED errno=%d(%s)", errno, errno_name(errno));
        close(fd);
        return (*env)->NewStringUTF(env, out);
    }

    /* Render a TWxTH ASCII thumbnail; ramp dark->light. Sample nibble per cell. */
    enum { TW = 48, TH = 64 };
    static const char ramp[] = "@%#*+=-:. "; /* idx0 = darkest(0x0), idx9 = white(0xF) */
    LOGI("HT-FB thumbnail %dx%d of live FB (%dx%d, 4bpp, stride=%zu); recognizable => offset0=live panel:",
         TW, TH, W, H, stride);
    size_t visible = stride * (size_t) H;
    if (visible > (size_t) EBC_FB_SIZE_GUESS) visible = (size_t) EBC_FB_SIZE_GUESS;
    char line[TW + 8];
    long nonwhite = 0;
    for (int ty = 0; ty < TH; ++ty) {
        int py = ty * H / TH;
        int m = 0;
        for (int tx = 0; tx < TW; ++tx) {
            int px = tx * W / TW;
            size_t bi = (size_t) py * stride + (size_t) (px / 2);
            int nib = 0xF;
            if (bi < visible) {
                uint8_t b = map[bi];
                nib = (px & 1) ? (b >> 4) : (b & 0x0F); /* low nibble = left px (assumption) */
            }
            if (nib != 0xF) ++nonwhite;
            line[m++] = ramp[(nib * 9) / 15];
        }
        line[m] = '\0';
        LOGI("  |%s|", line);
    }
    munmap(map, EBC_FB_SIZE_GUESS);
    close(fd);
    snprintf(out, sizeof(out), "HT-FB: read OK %dx%d 4bpp nonwhite_cells=%ld (thumbnail in logcat)",
             W, H, nonwhite);
    LOGI("%s", out);
    return (*env)->NewStringUTF(env, out);
}

/* Cheap reachability-only check: open()+close(), report -errno. (Unchanged behaviour.) */
JNIEXPORT jint JNICALL
Java_dev_jraghavan_inkread_penspike_EbcNative_canOpen(JNIEnv *env, jclass clazz) {
    (void) env; (void) clazz;
    int fd = open("/dev/ebc", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        int e = errno;
        LOGE("canOpen: open(/dev/ebc) errno=%d (%s)", e, errno_name(e));
        return -e;
    }
    close(fd);
    return 0;
}

/* Persistent session for the per-stroke A2 latency loop (64B struct). */
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
