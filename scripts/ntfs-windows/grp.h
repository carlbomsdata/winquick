#ifndef WQ_SHIM_GRP_H
#define WQ_SHIM_GRP_H
#include "wqtypes.h"
struct group { char *gr_name; char *gr_passwd; gid_t gr_gid; char **gr_mem; };
static __inline struct group *getgrnam(const char *n) { (void)n; return 0; }
static __inline struct group *getgrgid(gid_t g) { (void)g; return 0; }
#endif
