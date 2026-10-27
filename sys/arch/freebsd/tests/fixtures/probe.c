/* Native syscall probes; never uses a shell to decide whether access failed. */
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/mac.h>
#include <sys/procctl.h>
#include <sys/ptrace.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int
status(int value)
{
	if (value < 0) {
		fprintf(stderr, "%s\n", strerror(errno));
		return (1);
	}
	return (0);
}

static int
fork_exec_label(const char *self)
{
	pid_t child;
	int result;

	child = fork();
	if (child < 0)
		return (status(-1));
	if (child == 0) {
		execl(self, self, "label", (char *)NULL);
		_exit(126);
	}
	if (waitpid(child, &result, 0) != child)
		return (status(-1));
	return (WIFEXITED(result) && WEXITSTATUS(result) == 0 ? 0 : 1);
}

static int
signal_family(void)
{
	pid_t child;
	int result;

	child = fork();
	if (child < 0)
		return (status(-1));
	if (child == 0) {
		alarm(5);
		for (;;)
			pause();
	}
	if (kill(child, SIGKILL) != 0)
		return (status(-1));
	if (waitpid(child, &result, 0) != child)
		return (status(-1));
	return (WIFSIGNALED(result) && WTERMSIG(result) == SIGKILL ? 0 : 1);
}

static int
warden_check(const char *self, const char *marker)
{
	mac_t label;
	char *text;
	size_t size;
	int error, fd, jailed, n, pair[2];
	char byte;

	if (geteuid() == 0)
		return (1);
	jailed = 0;
	size = sizeof(jailed);
	if (sysctlbyname("security.jail.jailed", &jailed, &size, NULL, 0) != 0 ||
	    size != sizeof(jailed) || jailed != 1)
		return (1);
	if (mac_prepare(&label, "gulag") < 0 || mac_get_proc(label) < 0 ||
	    mac_to_text(label, &text) < 0)
		return (status(-1));
	n = strcmp(text, "gulag/restrict!v=1!net=deny");
	free(text);
	mac_free(label);
	if (n != 0)
		return (1);
	n = 0;
	if (procctl(P_PID, 0, PROC_NO_NEW_PRIVS_STATUS, &n) < 0 ||
	    n != PROC_NO_NEW_PRIVS_ENABLE)
		return (1);
	/* EROFS, not DAC denial or a missing parent, must explain refusal. */
	fd = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
	if (fd >= 0 || errno != EROFS)
		return (1);
	fd = socket(AF_INET, SOCK_STREAM, 0);
	if (fd >= 0 || errno != EPERM)
		return (1);
	fd = socket(AF_INET6, SOCK_STREAM, 0);
	if (fd >= 0 || errno != EPERM)
		return (1);
	fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (fd >= 0 || errno != EPERM)
		return (1);
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) < 0)
		return (status(-1));
	if (write(pair[0], "x", 1) != 1 || read(pair[1], &byte, 1) != 1 ||
	    byte != 'x')
		return (1);
	close(pair[0]);
	close(pair[1]);
	fd = open("/dev/null", O_WRONLY);
	if (fd < 0 || write(fd, "x", 1) != 1)
		return (1);
	close(fd);
	fd = open("/dev/random", O_RDONLY);
	if (fd < 0 || read(fd, &byte, 1) != 1)
		return (1);
	close(fd);
	fd = open("/dev/zfs", O_RDONLY);
	if (fd >= 0 || errno != ENOENT)
		return (1);
	if (mac_from_text(&label, "gulag/restrict!v=1") < 0)
		return (status(-1));
	n = mac_set_proc(label);
	error = errno;
	mac_free(label);
	if (n == 0 || error != EPERM)
		return (1);
	if (signal_family() != 0 || fork_exec_label(self) != 0)
		return (1);
	puts("warden-check: jailed non-root, readonly, net-deny, devices, fork/exec OK");
	return (0);
}

int
main(int argc, char **argv)
{
	int fd, pair[2], n;
	char byte;
	mac_t label;
	char *text;
	struct sockaddr_un address;

	if (argc < 2)
		return (2);
	if (strcmp(argv[1], "warden-check") == 0)
		return (argc == 3 ? warden_check(argv[0], argv[2]) : 2);
	if (strcmp(argv[1], "fork-exec-label") == 0)
		return (fork_exec_label(argv[0]));
	if (strcmp(argv[1], "signal-family") == 0)
		return (signal_family());
	if (strcmp(argv[1], "write") == 0) {
		fd = open(argv[2], O_WRONLY | O_CREAT | O_TRUNC, 0600);
		if (fd < 0)
			return (status(fd));
		n = write(fd, "confined\n", 9);
		close(fd);
		return (status(n));
	}
	if (strcmp(argv[1], "truncate") == 0)
		return (status(truncate(argv[2], 0)));
	if (strcmp(argv[1], "chmod") == 0)
		return (status(chmod(argv[2], 0600)));
	if (strcmp(argv[1], "mkdir") == 0)
		return (status(mkdir(argv[2], 0700)));
	if (strcmp(argv[1], "unlink") == 0)
		return (status(unlink(argv[2])));
	if (strcmp(argv[1], "rename") == 0)
		return (status(rename(argv[2], argv[3])));
	if (strcmp(argv[1], "link") == 0)
		return (status(link(argv[2], argv[3])));
	if (strcmp(argv[1], "tcp4") == 0)
		return (status(socket(AF_INET, SOCK_STREAM, 0)));
	if (strcmp(argv[1], "tcp6") == 0)
		return (status(socket(AF_INET6, SOCK_STREAM, 0)));
	if (strcmp(argv[1], "udp") == 0)
		return (status(socket(AF_INET, SOCK_DGRAM, 0)));
	if (strcmp(argv[1], "unix-connect") == 0) {
		memset(&address, 0, sizeof(address));
		address.sun_family = AF_UNIX;
		if (strlcpy(address.sun_path, argv[2], sizeof(address.sun_path)) >=
		    sizeof(address.sun_path))
			return (2);
		fd = socket(AF_UNIX, SOCK_STREAM, 0);
		if (fd < 0)
			return (status(fd));
		return (status(connect(fd, (struct sockaddr *)&address, sizeof(address))));
	}
	if (strcmp(argv[1], "socketpair") == 0) {
		if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) < 0)
			return (status(-1));
		if (write(pair[0], "x", 1) != 1 || read(pair[1], &byte, 1) != 1)
			return (status(-1));
		return (byte == 'x' ? 0 : 1);
	}
	if (strcmp(argv[1], "label") == 0) {
		if (mac_prepare(&label, "gulag") < 0 || mac_get_proc(label) < 0 ||
		    mac_to_text(label, &text) < 0)
			return (status(-1));
		puts(text);
		free(text);
		mac_free(label);
		return (0);
	}
	if (strcmp(argv[1], "setlabel") == 0) {
		if (mac_from_text(&label, argv[2]) < 0)
			return (status(-1));
		n = mac_set_proc(label);
		mac_free(label);
		return (status(n));
	}
	if (strcmp(argv[1], "hardening") == 0) {
		n = 0;
		if (procctl(P_PID, 0, PROC_NO_NEW_PRIVS_STATUS, &n) < 0)
			return (status(-1));
		return (n == PROC_NO_NEW_PRIVS_ENABLE ? 0 : 1);
	}
	if (strcmp(argv[1], "writefd") == 0)
		return (status(write(atoi(argv[2]), "escape", 6)));
	if (strcmp(argv[1], "signal") == 0)
		return (status(kill(atoi(argv[2]), SIGUSR1)));
	if (strcmp(argv[1], "ptrace") == 0)
		return (status(ptrace(PT_ATTACH, atoi(argv[2]), NULL, 0)));
	return (2);
}
