#include <libavutil/log.h>

/* Keep va_list in C: its ABI differs between MSVC, Intel and Apple ARM. */
static void (*log_sink)(int, const char *);

/* Set while a thread watches FFmpeg's verbose lines (see
   `diagnostics::FfmpegWatch`); the sink gives them only to that thread. Read
   from every thread that logs, so it is atomic. Relaxed is enough: a watching
   thread sets it before its own lines, and other threads' lines only decide
   what the sink drops. */
#ifdef _MSC_VER
#include <intrin.h>
static volatile long verbose_wanted;
static int verbose_is_wanted(void) { return _InterlockedCompareExchange(&verbose_wanted, 0, 0) != 0; }
static void set_verbose_wanted(int wanted) { _InterlockedExchange(&verbose_wanted, wanted ? 1 : 0); }
#else
#include <stdatomic.h>
static atomic_int verbose_wanted;
static int verbose_is_wanted(void) { return atomic_load_explicit(&verbose_wanted, memory_order_relaxed) != 0; }
static void set_verbose_wanted(int wanted) { atomic_store_explicit(&verbose_wanted, wanted ? 1 : 0, memory_order_relaxed); }
#endif

static void forward_log(void *context, int level, const char *format, va_list args) {
    if (!log_sink) return;
    if (level > AV_LOG_WARNING && !(level <= AV_LOG_VERBOSE && verbose_is_wanted())) return;
    char line[4096];
    int prefix = 1;
    int length = av_log_format_line2(context, level, format, args, line, sizeof(line), &prefix);
    if (length >= 0) log_sink(level, line);
}

void eternal_install_ffmpeg_logger(void (*sink)(int, const char *)) {
    log_sink = sink;
    /* Set once, before FFmpeg is used: FFmpeg's level is a plain int that
       every logging thread reads, so it is never changed afterwards. The
       callback drops what is not wanted. */
    av_log_set_level(AV_LOG_VERBOSE);
    av_log_set_callback(forward_log);
}

void eternal_ffmpeg_verbose(int wanted) {
    set_verbose_wanted(wanted);
}
