/* SPDX-License-Identifier: BSD-2-Clause */
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <string.h>
#include "policy_text.h"

int
main(void)
{
	static const char *invalid[] = {
		"", "unrestricted", "restrict", "restrict!v=0", "restrict!v=2",
		"restrict!v=1!w=/", "restrict!v=1!r=/tmp", "restrict!v=1!",
		"restrict!v=1!net=allow", "restrict!v=1!net=deny!",
		"restrict!v=1!net=deny!net=deny", "restrict!v=1!net=deny!w=/",
		"restrict!v=1!net=deny,other/label", "restrict!v=10",
	};
	bool deny = true;
	unsigned int i;

	assert(gulag_parse_policy_text("restrict!v=1", &deny) == 0);
	assert(!deny);
	assert(gulag_parse_policy_text("restrict!v=1!net=deny", &deny) == 0);
	assert(deny);
	for (i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
		deny = true;
		assert(gulag_parse_policy_text(invalid[i], &deny) == EINVAL);
		assert(deny); /* An invalid request must not change parser output. */
	}
	return (0);
}
