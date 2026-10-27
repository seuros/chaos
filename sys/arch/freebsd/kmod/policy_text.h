/* SPDX-License-Identifier: BSD-2-Clause */
#ifndef GULAG_POLICY_TEXT_H
#define GULAG_POLICY_TEXT_H

/* Exact ABI: never accept unsupported path rules and silently ignore them. */
static int
gulag_parse_policy_text(const char *text, bool *net_deny)
{
	if (strcmp(text, "restrict!v=1") == 0) {
		*net_deny = false;
		return (0);
	}
	if (strcmp(text, "restrict!v=1!net=deny") == 0) {
		*net_deny = true;
		return (0);
	}
	return (EINVAL);
}

#endif
