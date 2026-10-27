#include <sys/types.h>
#include <sys/queue.h>
#include <sys/ioctl.h>
#include <fs/devfs/devfs.h>
#include <stddef.h>
#include <stdio.h>

int
main(void)
{
	printf("%zu %zu %zu %zu %lu %lu\n", sizeof(struct devfs_rule),
	    offsetof(struct devfs_rule, dr_pathptrn),
	    offsetof(struct devfs_rule, dr_iacts),
	    offsetof(struct devfs_rule, dr_incset),
	    (unsigned long)DEVFSIO_RAPPLY, (unsigned long)DEVFSIO_RGETNEXT);
	return (0);
}
