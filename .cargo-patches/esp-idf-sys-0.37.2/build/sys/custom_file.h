/* PATCHED (MicroAgent): Stub sys/custom_file.h for __CUSTOM_FILE_IO__ mode.
 * 
 * When __CUSTOM_FILE_IO__ is defined, newlib's sys/reent.h expects this file
 * to exist and define __FILE. We provide a minimal definition that aliases
 * to picolibc's struct __file.
 */

#ifndef _SYS_CUSTOM_FILE_H_
#define _SYS_CUSTOM_FILE_H_

/* Use picolibc's file structure */
struct __file;
typedef struct __file __FILE;

#endif /* _SYS_CUSTOM_FILE_H_ */
