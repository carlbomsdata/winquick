/* POSIX declarations mingw-w64 does not ship, for building ntfsprogs natively.
 *
 * libntfs-3g is written for Unix. Building it for Windows with GCC leaves a
 * handful of POSIX names undeclared -- mostly in its security, ACL and daemon
 * code, which ntfscp and ntfscat never execute but still have to compile and
 * link. This header is force-included into every translation unit so those
 * names exist, without editing upstream source for anything that is purely a
 * missing declaration.
 *
 * Only two entries here have behaviour that matters: pread/pwrite, which do
 * the positioned I/O the device layer is built on, and fsync. Everything else
 * is a stub on a path that is never taken.
 *
 * Anything requiring a real change in upstream logic lives in
 * patches/ntfsprogs-windows.patch instead, where it is reviewable as a diff.
 */
#ifndef WQ_NTFS_WINDOWS_SHIM_H
#define WQ_NTFS_WINDOWS_SHIM_H

#include <errno.h>
#include <fcntl.h>
#include <io.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/types.h>

/* ---------------------------------------------------------------- identity */

typedef int uid_t;
typedef int gid_t;

static __inline uid_t getuid(void) { return 0; }
static __inline uid_t geteuid(void) { return 0; }
static __inline gid_t getgid(void) { return 0; }
static __inline gid_t getegid(void) { return 0; }

static __inline int chown(const char *p, uid_t u, gid_t g)
{ (void)p; (void)u; (void)g; return 0; }
static __inline int fchown(int fd, uid_t u, gid_t g)
{ (void)fd; (void)u; (void)g; return 0; }

/* ------------------------------------------------------- process lifecycle */
/* Referenced by ntfs-3g's daemon startup, which these two tools never reach. */

static __inline int fork(void) { errno = ENOSYS; return -1; }
static __inline int setsid(void) { errno = ENOSYS; return -1; }

/* ------------------------------------------------------------- mode bits */
/* Permission and node-type bits for things Windows has no concept of. An NTFS
 * image written by WinQuick never contains a symlink or a socket, but the code
 * that would name them still has to compile. */

#ifndef S_ISUID
#define S_ISUID  0004000
#endif
#ifndef S_ISGID
#define S_ISGID  0002000
#endif
#ifndef S_ISVTX
#define S_ISVTX  0001000
#endif
#ifndef S_IRWXU
#define S_IRWXU  0000700
#endif
#ifndef S_IRWXG
#define S_IRWXG  0000070
#endif
#ifndef S_IRWXO
#define S_IRWXO  0000007
#endif
#ifndef S_IFLNK
#define S_IFLNK  0120000
#endif
#ifndef S_IFSOCK
#define S_IFSOCK 0140000
#endif
#ifndef S_ISLNK
#define S_ISLNK(m)  (((m) & S_IFMT) == S_IFLNK)
#endif
#ifndef S_ISSOCK
#define S_ISSOCK(m) (((m) & S_IFMT) == S_IFSOCK)
#endif

static __inline int symlink(const char *a, const char *b)
{ (void)a; (void)b; errno = ENOSYS; return -1; }
static __inline int readlink(const char *p, char *b, size_t n)
{ (void)p; (void)b; (void)n; errno = ENOSYS; return -1; }

#ifndef major
#define major(d)        ((int)(((d) >> 8) & 0xff))
#endif
#ifndef minor
#define minor(d)        ((int)((d) & 0xff))
#endif
#ifndef makedev
#define makedev(ma, mi) ((((ma) & 0xff) << 8) | ((mi) & 0xff))
#endif

/* --------------------------------------------------------------- odds and ends */

/* ntfs-3g's compat.c defines ffs() when the platform lacks it; only the
 * declaration is missing. */
int ffs(int v);

/* Used to salt security-descriptor identifiers, where the distribution of the
 * values does not matter. */
static __inline long random(void) { return (long)rand(); }
static __inline void srandom(unsigned s) { srand(s); }

static __inline void setlinebuf(FILE *f) { setvbuf(f, (char *)0, _IOLBF, 0); }

/* --------------------------------------------------------- positioned I/O */
/*
 * mingw has neither pread nor pwrite, but it does have 64-bit seeks. Image
 * preparation is single-threaded and owns the file for the duration, so
 * save-seek-io-restore is exactly equivalent to the atomic POSIX call.
 *
 * These are functions and not macros on purpose: libntfs-3g's device
 * operations table has members named pread and pwrite, and a function-like
 * macro would rewrite those too.
 */
static __inline long long wq_pio(int fd, void *buf, unsigned long long count,
                                 long long offset, int writing)
{
	long long back = _lseeki64(fd, 0, SEEK_CUR);
	long long r;

	if (back < 0 || _lseeki64(fd, offset, SEEK_SET) < 0)
		return -1;
	r = writing ? _write(fd, buf, (unsigned)count)
	            : _read(fd, buf, (unsigned)count);
	_lseeki64(fd, back, SEEK_SET);
	return r;
}

static __inline long long pread(int fd, void *buf, unsigned long long count,
                                long long offset)
{
	return wq_pio(fd, buf, count, offset, 0);
}

static __inline long long pwrite(int fd, const void *buf,
                                 unsigned long long count, long long offset)
{
	return wq_pio(fd, (void *)buf, count, offset, 1);
}

/* ------------------------------------------------- syncing, locking, ioctl */
/*
 * The advisory lock is reported as taken and nothing is done: WinQuick points
 * these helpers at a plain image file it already owns, so there is no second
 * writer to advise about. ioctl exists only to fill the device-operations
 * table; the block-device queries behind it are meaningless for an image file,
 * so it fails the way a non-device would.
 */
#define F_RDLCK 0
#define F_WRLCK 1
#define F_UNLCK 2
#define F_GETLK 5
#define F_SETLK 6

struct flock {
	short l_type;
	short l_whence;
	long long l_start;
	long long l_len;
	int l_pid;
};

static __inline int fsync(int fd) { return _commit(fd); }
static __inline int fcntl(int fd, int cmd, ...) { (void)fd; (void)cmd; return 0; }
static __inline int ioctl(int fd, int req, ...)
{
	(void)fd; (void)req;
	errno = ENOTTY;
	return -1;
}

#endif /* WQ_NTFS_WINDOWS_SHIM_H */
