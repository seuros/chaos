/* SPDX-License-Identifier: BSD-2-Clause
 * Explicit root-only API reproducer. Creates one EMPTY jail, never attaches
 * or executes a command. Closing its owning descriptor removes it.
 */
#include <sys/param.h>
#include <sys/jail.h>
#include <sys/uio.h>
#include <err.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int
main(void)
{
	struct iovec create[8], query[2];
	char name[64];
	int owner = -1, copy, jid, got, result = 0;
	unsigned n = 0;

	if (geteuid() != 0)
		errx(1, "run explicitly as root; creates an empty temporary jail");
	snprintf(name, sizeof(name), "chaos-desc-copy-%d", getpid());
#define PAIR(key, value, length) do { \
	create[n].iov_base = (void *)(key); \
	create[n++].iov_len = strlen(key) + 1; \
	create[n].iov_base = (void *)(value); \
	create[n++].iov_len = (length); \
} while (0)
	PAIR("name", name, strlen(name) + 1);
	PAIR("path", "/", 2);
	PAIR("persist", NULL, 0);
	PAIR("desc", &owner, sizeof(owner));
	jid = jail_set(create, n, JAIL_CREATE | JAIL_OWN_DESC);
	if (jid < 0)
		err(1, "jail_set");
	if (owner < 0)
		errx(1, "kernel did not return owning descriptor");
	copy = owner;
	query[0].iov_base = (void *)"desc";
	query[0].iov_len = sizeof("desc");
	query[1].iov_base = &copy;
	query[1].iov_len = sizeof(copy);
	got = jail_get(query, 2, JAIL_USE_DESC | JAIL_GET_DESC);
	printf("jid=%d queried_jid=%d owner_fd=%d returned_fd=%d\n",
	    jid, got, owner, copy);
	if (got < 0) {
		warn("jail_get");
		result = 1;
	} else if (copy == owner) {
		fprintf(stderr, "GET_DESC did not return a new descriptor\n");
		result = 2;
	}
	if (copy >= 0 && copy != owner)
		close(copy);
	close(owner);
	return (result);
}
