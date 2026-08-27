#ifndef WQ_SHIM_SYSLOG_H
#define WQ_SHIM_SYSLOG_H
#define LOG_PID 0
#define LOG_DAEMON 0
#define LOG_USER 0
#define LOG_EMERG 0
#define LOG_ALERT 1
#define LOG_CRIT 2
#define LOG_ERR 3
#define LOG_WARNING 4
#define LOG_NOTICE 5
#define LOG_INFO 6
#define LOG_DEBUG 7
static __inline void openlog(const char *i, int o, int f) { (void)i;(void)o;(void)f; }
static __inline void closelog(void) { }
static __inline void syslog(int p, const char *f, ...) { (void)p;(void)f; }
#endif
