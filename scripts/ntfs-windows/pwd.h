#ifndef WQ_SHIM_PWD_H
#define WQ_SHIM_PWD_H
#include "wqtypes.h"
struct passwd { char *pw_name; char *pw_passwd; uid_t pw_uid; gid_t pw_gid;
                char *pw_gecos; char *pw_dir; char *pw_shell; };
static __inline struct passwd *getpwnam(const char *n) { (void)n; return 0; }
static __inline struct passwd *getpwuid(uid_t u) { (void)u; return 0; }
#endif
