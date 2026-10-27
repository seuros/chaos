/* Test the actual production accessor without loading a kernel module. */
#include <assert.h>
#include <stddef.h>
#include <stdint.h>

struct gulag_label {
	int marker;
};

struct label {
	intptr_t slots[4];
};

struct ucred {
	struct label *cr_label;
};

#define NOCRED ((struct ucred *)0)
#define FSCRED ((struct ucred *)-1)

static unsigned int reads;

static intptr_t
mac_label_get(struct label *label, int slot)
{
	/* Mirror the kernel contract that exposed the load-time panic. */
	assert(label != NULL);
	reads++;
	return (label->slots[slot]);
}

#include "label_slot.h"

int
main(void)
{
	struct label empty = {{0}};
	struct gulag_label policy = {42};
	struct label confined = {{0}};
	struct ucred unlabeled = {NULL};
	struct ucred unconfined = {&empty};
	struct ucred restricted = {&confined};

	/* Reproduce the pre-existing credential that panicked in the hook. */
	assert(gulag_credential_label(&unlabeled, 0) == NULL);
	assert(gulag_credential_label(NOCRED, 0) == NULL);
	assert(gulag_credential_label(FSCRED, 0) == NULL);
	assert(reads == 0);
	assert(gulag_label_get(NULL, 0) == NULL);
	assert(reads == 0);
	assert(gulag_label_get(&empty, 0) == NULL);
	assert(reads == 1);
	confined.slots[2] = (intptr_t)&policy;
	assert(gulag_label_get(&confined, 2) == &policy);
	assert(reads == 2);
	assert(gulag_credential_label(&unconfined, 0) == NULL);
	assert(gulag_credential_label(&restricted, 2) == &policy);
	assert(reads == 4);
	return (0);
}
