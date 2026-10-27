/* SPDX-License-Identifier: BSD-2-Clause */
#ifndef GULAG_LABEL_SLOT_H
#define GULAG_LABEL_SLOT_H

/*
 * Credentials created before a late-loaded MAC policy may have no label at
 * all (mac_cred_init() skips allocation while MPC_OBJECT_CRED is inactive).
 * mac_label_get(NULL, ...) panics; a missing label is an unconfined subject.
 *
 * Kept independently testable in userspace, without loading kernel code.
 * The includer provides struct label, struct ucred, struct gulag_label,
 * mac_label_get, NOCRED, FSCRED and NULL.
 */
static inline struct gulag_label *
gulag_label_get(struct label *label, int slot)
{
	if (label == NULL)
		return (NULL);
	return ((struct gulag_label *)mac_label_get(label, slot));
}

/* This is also used by privilege hooks immediately after a late load. */
static inline struct gulag_label *
gulag_credential_label(struct ucred *cred, int slot)
{
	if (cred == NOCRED || cred == FSCRED)
		return (NULL);
	return (gulag_label_get(cred->cr_label, slot));
}

#endif
