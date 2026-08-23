/* PATCHED (MicroAgent): Compatibility header to prevent __FILE typedef conflicts.
 * 
 * This header is forcibly included before any other headers via -include.
 * It defines __CUSTOM_FILE_IO__ to make newlib's sys/reent.h look for
 * sys/custom_file.h instead of defining __FILE itself. We provide that
 * file (in the build/sys/ directory) which aliases __FILE to picolibc's
 * struct __file.
 */

#ifndef _FILE_COMPAT_H_
#define _FILE_COMPAT_H_

/* Tell newlib's sys/reent.h to use sys/custom_file.h */
#define __CUSTOM_FILE_IO__ 1

#endif /* _FILE_COMPAT_H_ */
