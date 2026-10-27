/*-
 * SPDX-License-Identifier: BSD-2-Clause
 *
 * Copyright (c) 2026 Abdelkader Boudih <seuros@FreeBSD.org>
 *
 * GULAG: Glorious Unified Labor And Governance.
 * ABI 1: gulag/restrict!v=1[!net=deny]
 *
 * Experimental per-credential network/process restrictions. Filesystem
 * confinement belongs to the jail/nullfs supervisor, NOT this policy.
 * No vnode hooks, pathname rules, reverse lookups, or kernel API extensions.
 *
 * Labels are immutable, shared across fork/exec, and cannot be removed.
 * Unlabelled processes are unaffected. The exec helper must close inherited
 * descriptors other than intentional stdio capabilities and its exec-only fd.
 */
#include <sys/param.h>
#include <sys/domain.h>
#include <sys/kernel.h>
#include <sys/malloc.h>
#include <sys/module.h>
#include <sys/proc.h>
#include <sys/protosw.h>
#include <sys/refcount.h>
#include <sys/sbuf.h>
#include <sys/socket.h>
#include <sys/socketvar.h>
#include <sys/systm.h>
#include <sys/sysctl.h>
#include <sys/ucred.h>
#include <machine/atomic.h>
#include <security/mac/mac_policy.h>

#define GULAG_DRAINING (1U << 31)

static MALLOC_DEFINE(M_GULAG, "gulag", "mac_gulag labels");
static int gulag_slot;
static int gulag_abi = 1;
static int gulag_enabled = 1;
static volatile u_int gulag_labels;

static SYSCTL_NODE(_security_mac, OID_AUTO, gulag,
    CTLFLAG_RD | CTLFLAG_MPSAFE, 0, "Experimental network/process policy");
SYSCTL_INT(_security_mac_gulag, OID_AUTO, abi_version, CTLFLAG_RD,
    &gulag_abi, 0, "Userspace label ABI (no filesystem enforcement)");
/* Existing restrictions must not be disabled through a writable sysctl. */
SYSCTL_INT(_security_mac_gulag, OID_AUTO, enabled, CTLFLAG_RD,
    &gulag_enabled, 0, "Network/process enforcement active");

struct gulag_label {
	volatile u_int refs;
	bool net_deny;
};

#include "label_slot.h"
#include "policy_text.h"

#define SLOT(l) gulag_label_get((l), gulag_slot)
#define SET(l, p) mac_label_set((l), gulag_slot, (intptr_t)(p))

static struct gulag_label *
gulag_credential(struct ucred *cred)
{
	return (gulag_credential_label(cred, gulag_slot));
}

static struct gulag_label *
gulag_alloc(void)
{
	struct gulag_label *al;
	u_int n;

	do {
		n = atomic_load_acq_int(&gulag_labels);
		if (n & GULAG_DRAINING)
			return (NULL);
	} while (!atomic_cmpset_int(&gulag_labels, n, n + 1));
	al = malloc(sizeof(*al), M_GULAG, M_WAITOK | M_ZERO);
	refcount_init(&al->refs, 1);
	return (al);
}

static void
gulag_release(struct gulag_label *al)
{
	if (al == NULL || !refcount_release(&al->refs))
		return;
	free(al, M_GULAG);
	atomic_subtract_int(&gulag_labels, 1);
}

static void
gulag_cred_init(struct label *label)
{
	SET(label, NULL);
}

static void
gulag_cred_destroy(struct label *label)
{
	gulag_release(SLOT(label));
	SET(label, NULL);
}

static void
gulag_cred_copy(struct label *src, struct label *dst)
{
	struct gulag_label *al = SLOT(src);

	/* Late-load credentials may have no label. Never allocate in NOSLEEP. */
	if (dst == NULL) {
		if (al != NULL)
			panic("gulag: copying confined credential without destination label");
		return;
	}
	if (al != NULL)
		refcount_acquire(&al->refs);
	gulag_release(SLOT(dst));
	SET(dst, al);
}

static int
gulag_internalize(struct label *label, char *element, char *text, int *claimed)
{
	struct gulag_label *al;
	bool net_deny;
	int error;

	if (strcmp(element, "gulag") != 0)
		return (0);
	(*claimed)++;
	error = gulag_parse_policy_text(text, &net_deny);
	if (error != 0)
		return (error);
	al = gulag_alloc();
	if (al == NULL)
		return (EBUSY);
	al->net_deny = net_deny;
	gulag_release(SLOT(label));
	SET(label, al);
	return (0);
}

static int
gulag_externalize(struct label *label, char *element, struct sbuf *sb,
    int *claimed)
{
	struct gulag_label *al;

	if (strcmp(element, "gulag") != 0)
		return (0);
	(*claimed)++;
	al = SLOT(label);
	if (al == NULL)
		sbuf_printf(sb, "unrestricted");
	else
		sbuf_printf(sb, "restrict!v=1%s", al->net_deny ? "!net=deny" : "");
	return (0);
}

static int
gulag_relabel_check(struct ucred *cred, struct label *label)
{
	return (gulag_credential(cred) == NULL ? 0 : EPERM);
}

static void
gulag_relabel(struct ucred *cred, struct label *label)
{
	if (SLOT(label) != NULL)
		gulag_cred_copy(label, cred->cr_label);
}

static int
gulag_priv(struct ucred *cred, int priv)
{
	return (gulag_credential(cred) == NULL ? 0 : EPERM);
}

static int
gulag_debug(struct ucred *cred, struct proc *p)
{
	return (gulag_credential(cred) == NULL ? 0 : EPERM);
}

static int
gulag_signal(struct ucred *cred, struct proc *p, int signal)
{
	struct gulag_label *al = gulag_credential(cred);
	return (al == NULL || al == gulag_credential(p->p_ucred) ? 0 : EPERM);
}

static bool
gulag_net(struct ucred *cred)
{
	struct gulag_label *al = gulag_credential(cred);
	return (al != NULL && al->net_deny);
}

static int
gulag_socket_create(struct ucred *cred, int domain, int type, int protocol)
{
	return (gulag_net(cred) && domain != AF_LOCAL ? EPERM : 0);
}

static int
gulag_socket_address(struct ucred *cred, struct socket *so, struct label *l,
    struct sockaddr *sa)
{
	/* socketpair remains usable; ambient IPC brokers do not. */
	return (gulag_net(cred) ? EPERM : 0);
}

static int
gulag_socket_send(struct ucred *cred, struct socket *so, struct label *l)
{
	if (gulag_net(cred) &&
	    (so->so_proto->pr_domain->dom_family != AF_LOCAL ||
	    so->so_type == SOCK_DGRAM || !(so->so_state & SS_ISCONNECTED)))
		return (EPERM);
	return (0);
}

static int
gulag_socket_accept(struct ucred *cred, struct socket *so, struct label *l)
{
	return (gulag_net(cred) ? EPERM : 0);
}

static struct mac_policy_ops gulag_ops = {
	.mpo_cred_init_label = gulag_cred_init,
	.mpo_cred_destroy_label = gulag_cred_destroy,
	.mpo_cred_copy_label = gulag_cred_copy,
	.mpo_cred_internalize_label = gulag_internalize,
	.mpo_cred_externalize_label = gulag_externalize,
	.mpo_cred_check_relabel = gulag_relabel_check,
	.mpo_cred_relabel = gulag_relabel,
	.mpo_priv_check = gulag_priv,
	.mpo_proc_check_debug = gulag_debug,
	.mpo_proc_check_signal = gulag_signal,
	.mpo_socket_check_create = gulag_socket_create,
	.mpo_socket_check_connect = gulag_socket_address,
	.mpo_socket_check_bind = gulag_socket_address,
	.mpo_socket_check_send = gulag_socket_send,
	.mpo_socket_check_accept = gulag_socket_accept,
};

static struct mac_policy_conf gulag_conf = {
	.mpc_name = "mac_gulag",
	.mpc_fullname = "GULAG: Glorious Unified Labor And Governance (experimental ABI 1)",
	.mpc_ops = &gulag_ops,
	.mpc_loadtime_flags = MPC_LOADTIME_FLAG_UNLOADOK,
	.mpc_field_off = &gulag_slot,
};

static int
gulag_modevent(module_t mod, int type, void *data)
{
	int error;

	if (type == MOD_UNLOAD &&
	    !atomic_cmpset_int(&gulag_labels, 0, GULAG_DRAINING))
		return (EBUSY);
	error = mac_policy_modevent(mod, type, &gulag_conf);
	if (type == MOD_UNLOAD && error != 0)
		atomic_store_rel_int(&gulag_labels, 0);
	return (error);
}

static moduledata_t gulag_mod = {
	"mac_gulag", gulag_modevent, NULL
};
MODULE_DEPEND(mac_gulag, kernel_mac_support, MAC_VERSION, MAC_VERSION, MAC_VERSION);
DECLARE_MODULE(mac_gulag, gulag_mod, SI_SUB_MAC_POLICY, SI_ORDER_MIDDLE);
