// The user namespace checks that Chromium runs before it starts the zygote.
//
// sandbox::Credentials::CanCreateProcessInNewUserNS() clones a child into a new
// user namespace with the raw clone system call, from a process that already has
// threads. The child maps its own IDs, drops all capabilities and must still be
// able to create a nested user namespace. NamespaceSandbox then starts processes
// in new user, PID and network namespaces the same way.
//
// Reference: sandbox/linux/services/credentials.cc and
// sandbox/linux/services/namespace_sandbox.cc in Chromium.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/capability.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
    STEP_SETGROUPS = 10,
    STEP_UID_MAP,
    STEP_GID_MAP,
    STEP_DROP_CAPS,
    STEP_CAPS_LEFT,
    STEP_NESTED,
    STEP_PID_ONE,
    STEP_SINGLE_THREADED,
};

static const char *step_names[] = {
    [STEP_SETGROUPS] = "write setgroups",
    [STEP_UID_MAP] = "write uid_map",
    [STEP_GID_MAP] = "write gid_map",
    [STEP_DROP_CAPS] = "drop capabilities",
    [STEP_CAPS_LEFT] = "capabilities left after dropping",
    [STEP_NESTED] = "nested unshare(CLONE_NEWUSER)",
    [STEP_PID_ONE] = "PID 1 in the new PID namespace",
    [STEP_SINGLE_THREADED] = "link count of /proc/self/task in a single-threaded child",
};

// Chromium counts threads from the link count of /proc/self/task: two links for
// the directory and its parent entry, and one for each thread.
// Reference: sandbox/linux/services/thread_helpers.cc in Chromium.
static long task_links(void) {
    struct stat task;
    if (stat("/proc/self/task/", &task) != 0) return -1;
    return (long)task.st_nlink;
}

static int write_file(const char *path, const char *text) {
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) return -1;
    ssize_t length = (ssize_t)strlen(text);
    ssize_t written = write(fd, text, (size_t)length);
    int saved = errno;
    close(fd);
    errno = saved;
    return written == length ? 0 : -1;
}

// Exits with the failing step, and reports the errno through a pipe.
static int report_fd = -1;

static void fail(int step) {
    int error = errno;
    if (write(report_fd, &error, sizeof(error)) < 0) {
        // The parent reports a missing errno.
    }
    _exit(step);
}

static void child(uid_t uid, gid_t gid, int check_pid_one) {
    char map[64];
    if (check_pid_one && syscall(SYS_getpid) != 1) fail(STEP_PID_ONE);
    if (task_links() != 3) {
        errno = 0;
        fail(STEP_SINGLE_THREADED);
    }
    if (write_file("/proc/self/setgroups", "deny") != 0) fail(STEP_SETGROUPS);
    snprintf(map, sizeof(map), "%u %u 1\n", (unsigned)uid, (unsigned)uid);
    if (write_file("/proc/self/uid_map", map) != 0) fail(STEP_UID_MAP);
    snprintf(map, sizeof(map), "%u %u 1\n", (unsigned)gid, (unsigned)gid);
    if (write_file("/proc/self/gid_map", map) != 0) fail(STEP_GID_MAP);

    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct data[2];
    memset(data, 0, sizeof(data));
    if (syscall(SYS_capset, &header, data) != 0) fail(STEP_DROP_CAPS);
    if (prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) != 0) fail(STEP_DROP_CAPS);
    if (syscall(SYS_capget, &header, data) != 0) fail(STEP_DROP_CAPS);
    if (data[0].effective || data[0].permitted || data[1].effective || data[1].permitted) {
        errno = 0;
        fail(STEP_CAPS_LEFT);
    }
    if (syscall(SYS_unshare, CLONE_NEWUSER) != 0) fail(STEP_NESTED);
    _exit(0);
}

static void *idle(void *unused) {
    (void)unused;
    pause();
    return NULL;
}

// Clones like Chromium and reports the result as one JSON line.
static int check(const char *name, unsigned long flags) {
    int pipe_fds[2];
    if (pipe2(pipe_fds, O_CLOEXEC) != 0) return 1;
    uid_t uid = geteuid();
    gid_t gid = getegid();
    report_fd = pipe_fds[1];
    long pid = syscall(SYS_clone, flags | SIGCHLD, NULL, NULL, NULL, NULL);
    if (pid == 0) child(uid, gid, (flags & CLONE_NEWPID) != 0);
    int clone_errno = errno;
    close(pipe_fds[1]);
    if (pid < 0) {
        printf("{\"probe\": \"chromium_%s\", \"status\": \"failed\", \"error\": \"clone: %s\"}\n", name,
               strerror(clone_errno));
        close(pipe_fds[0]);
        return 1;
    }
    int status = 0;
    while (waitpid((pid_t)pid, &status, 0) < 0 && errno == EINTR) {
    }
    int child_errno = 0;
    ssize_t got = read(pipe_fds[0], &child_errno, sizeof(child_errno));
    close(pipe_fds[0]);
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("{\"probe\": \"chromium_%s\", \"status\": \"passed\"}\n", name);
        return 0;
    }
    int step = WIFEXITED(status) ? WEXITSTATUS(status) : 0;
    const char *what = step >= STEP_SETGROUPS && step <= STEP_SINGLE_THREADED ? step_names[step] : "the child crashed";
    printf("{\"probe\": \"chromium_%s\", \"status\": \"failed\", \"error\": \"%s: %s\"}\n", name, what,
           got == (ssize_t)sizeof(child_errno) && child_errno ? strerror(child_errno) : "no error number");
    return 1;
}

// The zygote handshake: the browser starts the zygote by executing /proc/self/exe
// in new namespaces, and the zygote sends a boot message over a SOCK_SEQPACKET
// socket. The browser learns the zygote's PID from SCM_CREDENTIALS.
// Reference: content/browser/zygote_host/zygote_host_impl_linux.cc in Chromium.
static const char boot_message[] = "ZYGOTE_BOOT";
static const char hello_message[] = "ZYGOTE_OK";
enum { ZYGOTE_FD = 3 };

// Reports the failing step to the browser side and ends the zygote.
static void zygote_fail(const char *step) {
    char message[96];
    int length = snprintf(message, sizeof(message), "FAIL %s: %s", step, strerror(errno));
    if (send(ZYGOTE_FD, message, (size_t)length + 1, MSG_NOSIGNAL) < 0) {
        // The browser side reports the closed socket.
    }
    _exit(1);
}

// Runs in a vfork-like child that shares the filesystem state with the zygote.
// Reference: ChrootToSafeEmptyDir() in sandbox/linux/services/credentials.cc.
static int chroot_to_fdinfo(void *unused) {
    (void)unused;
    if (syscall(SYS_chroot, "/proc/self/fdinfo/") != 0) _exit(1);
    if (syscall(SYS_chdir, "/") != 0) _exit(2);
    _exit(0);
}

static long task_links_at(int proc_fd) {
    struct stat task;
    if (fstatat(proc_fd, "self/task/", &task, 0) != 0) return -1;
    return (long)task.st_nlink;
}

// The sandboxed zygote engages the namespace sandbox before it says hello.
// Reference: SandboxLinux::EngageNamespaceSandboxInternal() and
// CreateInitProcessReaper() in Chromium.
static void engage_namespace_sandbox(void) {
    int proc_fd = open("/proc", O_DIRECTORY | O_RDONLY | O_CLOEXEC);
    if (proc_fd < 0) zygote_fail("open /proc");
    if (getpid() != 1) zygote_fail("PID 1 in the new PID namespace");
    errno = 0;
    if (task_links_at(proc_fd) != 3) zygote_fail("single-threaded check");

    // Credentials::MoveToNewUserNS(): a nested user namespace after execve.
    uid_t uid = getuid();
    gid_t gid = getgid();
    char map[64];
    if (syscall(SYS_unshare, CLONE_NEWUSER) != 0) zygote_fail("nested unshare(CLONE_NEWUSER)");
    if (write_file("/proc/self/setgroups", "deny") != 0) zygote_fail("nested setgroups");
    snprintf(map, sizeof(map), "%u %u 1\n", (unsigned)gid, (unsigned)gid);
    if (write_file("/proc/self/gid_map", map) != 0) zygote_fail("nested gid_map");
    snprintf(map, sizeof(map), "%u %u 1\n", (unsigned)uid, (unsigned)uid);
    if (write_file("/proc/self/uid_map", map) != 0) zygote_fail("nested uid_map");

    // Credentials::DropFileSystemAccess(): a helper that shares the memory and
    // the filesystem state moves the root into a directory that disappears.
    static char stack[1 << 16] __attribute__((aligned(16)));
    static char tls[1 << 12] __attribute__((aligned(64)));
    pid_t helper = clone(chroot_to_fdinfo, stack + sizeof(stack),
                         CLONE_FS | CLONE_VM | CLONE_VFORK | CLONE_SETTLS | SIGCHLD, NULL, NULL, tls, NULL);
    int clone_errno = errno;
    if (helper < 0) {
        errno = clone_errno;
        zygote_fail("clone the chroot helper");
    }
    int status = 0;
    pid_t waited = waitpid(helper, &status, 0);
    int wait_errno = errno;
    if (waited != helper) {
        char step[80];
        snprintf(step, sizeof(step), "waitpid(%d) returned %d", (int)helper, (int)waited);
        errno = wait_errno;
        zygote_fail(step);
    }
    errno = 0;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) zygote_fail("chroot helper");
    struct stat proc;
    if (stat("/proc", &proc) == 0) {
        errno = 0;
        zygote_fail("/proc is still reachable");
    }

    // Credentials::SetCapabilities(): keep CAP_SYS_ADMIN and CAP_SYS_CHROOT.
    errno = 0;
    if (task_links_at(proc_fd) != 3) zygote_fail("single-threaded check through the /proc descriptor");
    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct data[2];
    memset(data, 0, sizeof(data));
    data[0].effective = data[0].permitted = (1u << CAP_SYS_ADMIN) | (1u << CAP_SYS_CHROOT);
    if (syscall(SYS_capset, &header, data) != 0) zygote_fail("keep CAP_SYS_ADMIN and CAP_SYS_CHROOT");

    // CreateInitProcessReaper(): PID 1 forks and stays as the reaper.
    pid_t child = fork();
    if (child < 0) zygote_fail("fork the reaper");
    if (child > 0) {
        struct sigaction action;
        memset(&action, 0, sizeof(action));
        action.sa_handler = SIG_DFL;
        action.sa_flags = SA_NOCLDSTOP;
        if (sigaction(SIGCHLD, &action, NULL) != 0) zygote_fail("reaper SIGCHLD handler");
        while (waitpid(child, &status, 0) < 0 && errno == EINTR) {
        }
        _exit(WIFEXITED(status) ? WEXITSTATUS(status) : 1);
    }
}

// Chromium runs helpers with glibc's clone() and its own stack, sharing the memory
// of the caller, and waits for them with waitpid().
static int exit_seven(void *unused) {
    (void)unused;
    syscall(SYS_exit, 7);
    return 0;
}

static int check_clone_helpers(void) {
    static const struct {
        const char *name;
        int flags;
    } variants[] = {
        {"plain", 0},
        {"vm", CLONE_VM},
        {"vm_vfork", CLONE_VM | CLONE_VFORK},
        {"vm_vfork_settls", CLONE_VM | CLONE_VFORK | CLONE_SETTLS},
        {"fs_vm_vfork_settls", CLONE_FS | CLONE_VM | CLONE_VFORK | CLONE_SETTLS},
    };
    static char stack[1 << 16] __attribute__((aligned(16)));
    static char tls[1 << 12] __attribute__((aligned(64)));
    int failed = 0;
    for (size_t index = 0; index < sizeof(variants) / sizeof(variants[0]); index++) {
        int flags = variants[index].flags;
        pid_t pid = clone(exit_seven, stack + sizeof(stack), flags | SIGCHLD, NULL, NULL,
                          (flags & CLONE_SETTLS) ? tls : NULL, NULL);
        int clone_errno = errno;
        int status = 0;
        pid_t waited = pid > 0 ? waitpid(pid, &status, 0) : -1;
        int wait_errno = errno;
        if (pid > 0 && waited == pid && WIFEXITED(status) && WEXITSTATUS(status) == 7) {
            printf("{\"probe\": \"chromium_clone_%s\", \"status\": \"passed\"}\n", variants[index].name);
            continue;
        }
        failed = 1;
        printf("{\"probe\": \"chromium_clone_%s\", \"status\": \"failed\", \"error\": \"clone %d (%s), waitpid %d (%s), "
               "status %#x\"}\n",
               variants[index].name, (int)pid, strerror(clone_errno), (int)waited, strerror(wait_errno),
               (unsigned)status);
    }
    return failed;
}

// Mojo, the IPC of Chromium, passes UNIX sockets over UNIX sockets.
static int send_socket(int channel, int socket_fd) {
    char byte = 'x';
    struct iovec iov = {&byte, 1};
    union {
        struct cmsghdr header;
        char space[CMSG_SPACE(sizeof(int))];
    } control;
    memset(&control, 0, sizeof(control));
    struct msghdr message = {0};
    message.msg_iov = &iov;
    message.msg_iovlen = 1;
    message.msg_control = control.space;
    message.msg_controllen = sizeof(control.space);
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(header), &socket_fd, sizeof(int));
    return sendmsg(channel, &message, MSG_NOSIGNAL) == 1 ? 0 : -1;
}

static int receive_socket(int channel) {
    char byte;
    struct iovec iov = {&byte, 1};
    union {
        struct cmsghdr header;
        char space[CMSG_SPACE(sizeof(int))];
    } control;
    struct msghdr message = {0};
    message.msg_iov = &iov;
    message.msg_iovlen = 1;
    message.msg_control = control.space;
    message.msg_controllen = sizeof(control.space);
    if (recvmsg(channel, &message, MSG_CMSG_CLOEXEC) != 1) return -1;
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    if (!header || header->cmsg_type != SCM_RIGHTS) return -1;
    int fd;
    memcpy(&fd, CMSG_DATA(header), sizeof(int));
    return fd;
}

static void socket_passing_child(void) {
    int channel[2], passed[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, channel) != 0 || socketpair(AF_UNIX, SOCK_STREAM, 0, passed) != 0)
        _exit(10);
    if (send_socket(channel[0], passed[0]) != 0) _exit(11);
    int received = receive_socket(channel[1]);
    if (received < 0) _exit(12);
    // The received socket is connected to the same peer.
    char byte = 0;
    if (write(received, "y", 1) != 1 || read(passed[1], &byte, 1) != 1 || byte != 'y') _exit(13);
    close(received);

    // The sockets in flight of a user are limited by its RLIMIT_NOFILE.
    struct rlimit limit = {32, 32};
    if (setrlimit(RLIMIT_NOFILE, &limit) != 0) _exit(14);
    int sent = 0;
    while (sent < 200 && send_socket(channel[0], passed[0]) == 0) sent++;
    if (sent == 200 || errno != ETOOMANYREFS) _exit(15);
    // Receiving the sockets releases them.
    for (int index = 0; index < sent; index++) {
        int fd = receive_socket(channel[1]);
        if (fd < 0) _exit(16);
        close(fd);
    }
    if (send_socket(channel[0], passed[0]) != 0) _exit(17);
    _exit(0);
}

static int check_socket_passing(void) {
    pid_t child = fork();
    if (child == 0) socket_passing_child();
    int status = 0;
    while (child > 0 && waitpid(child, &status, 0) < 0 && errno == EINTR) {
    }
    if (child > 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        puts("{\"probe\": \"chromium_socket_passing\", \"status\": \"passed\"}");
        return 0;
    }
    printf("{\"probe\": \"chromium_socket_passing\", \"status\": \"failed\", \"error\": \"step %d\"}\n",
           WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return 1;
}

// The network service of Chromium reads the addresses and links over netlink before
// it reports the connection type, and the browser waits for that report.
// Reference: net/base/address_tracker_linux.cc in Chromium.
static int netlink_dump(int fd, int type, char *error, size_t size) {
    struct {
        struct nlmsghdr header;
        struct rtgenmsg message;
    } request;
    memset(&request, 0, sizeof(request));
    request.header.nlmsg_len = NLMSG_LENGTH(sizeof(request.message));
    request.header.nlmsg_type = (unsigned short)type;
    request.header.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    request.message.rtgen_family = AF_UNSPEC;
    struct sockaddr_nl peer = {.nl_family = AF_NETLINK};
    if (sendto(fd, &request, request.header.nlmsg_len, 0, (struct sockaddr *)&peer, sizeof(peer)) < 0) {
        snprintf(error, size, "sendto type %d: %s", type, strerror(errno));
        return -1;
    }
    char buffer[8192] __attribute__((aligned(4)));
    int done = 0;
    for (int first = 1;; first = 0) {
        ssize_t length = recv(fd, buffer, sizeof(buffer), first ? 0 : MSG_DONTWAIT);
        if (length < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) break;
            snprintf(error, size, "recv type %d: %s", type, strerror(errno));
            return -1;
        }
        if (length == 0) {
            snprintf(error, size, "recv type %d: end of stream", type);
            return -1;
        }
        for (struct nlmsghdr *header = (struct nlmsghdr *)buffer; NLMSG_OK(header, (unsigned)length);
             header = NLMSG_NEXT(header, length)) {
            if (header->nlmsg_type == NLMSG_DONE) done = 1;
            if (header->nlmsg_type == NLMSG_ERROR) {
                snprintf(error, size, "type %d: netlink error", type);
                return -1;
            }
        }
    }
    if (!done) {
        snprintf(error, size, "type %d: no NLMSG_DONE", type);
        return -1;
    }
    return 0;
}

static int check_address_tracker(void) {
    char error[128] = "";
    int pair[2];
    char byte;
    // A receive with MSG_DONTWAIT on a blocking socket returns at once.
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) {
        snprintf(error, sizeof(error), "socketpair: %s", strerror(errno));
    } else {
        errno = 0;
        if (recv(pair[0], &byte, 1, MSG_DONTWAIT) != -1 || errno != EAGAIN) {
            snprintf(error, sizeof(error), "MSG_DONTWAIT on an empty socket: %s", strerror(errno));
        }
        close(pair[0]);
        close(pair[1]);
    }
    if (!error[0]) {
        int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
        struct sockaddr_nl local = {.nl_family = AF_NETLINK};
        local.nl_groups = RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR | RTMGRP_NOTIFY | RTMGRP_LINK;
        if (fd < 0 || bind(fd, (struct sockaddr *)&local, sizeof(local)) != 0) {
            snprintf(error, sizeof(error), "netlink socket: %s", strerror(errno));
        } else if (netlink_dump(fd, RTM_GETADDR, error, sizeof(error)) == 0) {
            netlink_dump(fd, RTM_GETLINK, error, sizeof(error));
        }
        if (fd >= 0) close(fd);
    }
    if (error[0]) {
        printf("{\"probe\": \"chromium_address_tracker\", \"status\": \"failed\", \"error\": \"%s\"}\n", error);
        return 1;
    }
    puts("{\"probe\": \"chromium_address_tracker\", \"status\": \"passed\"}");
    return 0;
}

// Zygote children rewrite their title over the argument and environment strings, which
// must be contiguous and in order. /proc/[pid]/cmdline then shows the new title, and
// tools find the renderers by it.
// Reference: base/process/set_process_title_linux.cc in Chromium.
extern char **environ;

static void process_title_child(char **argv) {
    char *end = argv[0];
    for (int index = 0; argv[index]; index++) {
        if (argv[index] != end) _exit(10);
        end = argv[index] + strlen(argv[index]) + 1;
    }
    char *argv_end = end;
    for (int index = 0; environ[index]; index++) {
        if (environ[index] != end) _exit(11);
        end = environ[index] + strlen(environ[index]) + 1;
    }
    // A title longer than the argument strings, as Chromium writes for its children.
    size_t available = (size_t)(end - argv[0]);
    size_t length = (size_t)(argv_end - argv[0]) + 16;
    if (length + 1 > available) _exit(12);
    char title[512];
    if (length + 1 > sizeof(title)) _exit(13);
    for (size_t index = 0; index < length; index++) title[index] = (char)('a' + index % 26);
    title[length] = 0;
    memcpy(argv[0], title, length + 1);
    memset(argv[0] + length + 1, 0, available - length - 1);

    char cmdline[1024];
    int fd = open("/proc/self/cmdline", O_RDONLY);
    if (fd < 0) _exit(14);
    ssize_t got = read(fd, cmdline, sizeof(cmdline));
    close(fd);
    if (got <= 0 || (size_t)got < length || memcmp(cmdline, title, length) != 0) _exit(15);
    _exit(0);
}

static int check_process_title(char **argv) {
    pid_t child = fork();
    if (child == 0) process_title_child(argv);
    int status = 0;
    while (child > 0 && waitpid(child, &status, 0) < 0 && errno == EINTR) {
    }
    if (child > 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        puts("{\"probe\": \"chromium_process_title\", \"status\": \"passed\"}");
        return 0;
    }
    printf("{\"probe\": \"chromium_process_title\", \"status\": \"failed\", \"error\": \"step %d\"}\n",
           WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return 1;
}

static int zygote_main(int sandboxed) {
    ssize_t sent = send(ZYGOTE_FD, boot_message, sizeof(boot_message), MSG_NOSIGNAL);
    if (sent != (ssize_t)sizeof(boot_message)) return 1;
    if (sandboxed) engage_namespace_sandbox();
    sent = send(ZYGOTE_FD, hello_message, sizeof(hello_message), MSG_NOSIGNAL);
    return sent == (ssize_t)sizeof(hello_message) ? 0 : 1;
}

static int check_zygote(const char *name, unsigned long flags) {
    int fds[2];
    int enable = 1;
    char error[160] = "";
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, fds) != 0 ||
        setsockopt(fds[0], SOL_SOCKET, SO_PASSCRED, &enable, sizeof(enable)) != 0) {
        snprintf(error, sizeof(error), "socket setup: %s", strerror(errno));
        goto report;
    }
    uid_t uid = geteuid();
    gid_t gid = getegid();
    long pid = syscall(SYS_clone, flags | SIGCHLD, NULL, NULL, NULL, NULL);
    if (pid == 0) {
        char map[64];
        if (flags & CLONE_NEWUSER) {
            snprintf(map, sizeof(map), "%u %u 1\n", (unsigned)uid, (unsigned)uid);
            if (write_file("/proc/self/setgroups", "deny") != 0 || write_file("/proc/self/uid_map", map) != 0) _exit(120);
            snprintf(map, sizeof(map), "%u %u 1\n", (unsigned)gid, (unsigned)gid);
            if (write_file("/proc/self/gid_map", map) != 0) _exit(121);
        }
        if (dup2(fds[1], ZYGOTE_FD) != ZYGOTE_FD) _exit(122);
        execl("/proc/self/exe", "chromium-userns", (flags & CLONE_NEWUSER) ? "--zygote-sandboxed" : "--zygote",
              (char *)NULL);
        dprintf(STDERR_FILENO, "exec /proc/self/exe: %s\n", strerror(errno));
        _exit(123);
    }
    close(fds[1]);
    if (pid < 0) {
        snprintf(error, sizeof(error), "clone: %s", strerror(errno));
        close(fds[0]);
        goto report;
    }

    char buffer[64];
    union {
        struct cmsghdr header;
        char space[CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int) * 8)];
    } control;
    struct iovec iov = {buffer, sizeof(buffer)};
    struct msghdr message = {0};
    message.msg_iov = &iov;
    message.msg_iovlen = 1;
    message.msg_control = control.space;
    message.msg_controllen = sizeof(control.space);
    ssize_t received = recvmsg(fds[0], &message, 0);
    int receive_errno = errno;
    pid_t sender = -1;
    for (struct cmsghdr *header = CMSG_FIRSTHDR(&message); received > 0 && header;
         header = CMSG_NXTHDR(&message, header)) {
        if (header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_CREDENTIALS) {
            struct ucred credentials;
            memcpy(&credentials, CMSG_DATA(header), sizeof(credentials));
            sender = credentials.pid;
        }
    }
    // The zygote says hello once it is ready, or names the step that failed.
    char hello[128] = "";
    ssize_t hello_length = received > 0 ? recv(fds[0], hello, sizeof(hello) - 1, 0) : -1;
    close(fds[0]);
    int status = 0;
    while (waitpid((pid_t)pid, &status, 0) < 0 && errno == EINTR) {
    }

    if (received < 0) {
        snprintf(error, sizeof(error), "recvmsg: %s", strerror(receive_errno));
    } else if (received == 0) {
        snprintf(error, sizeof(error), "the zygote closed the socket, wait status %#x", (unsigned)status);
    } else if (received != (ssize_t)sizeof(boot_message) || memcmp(buffer, boot_message, sizeof(boot_message)) != 0) {
        snprintf(error, sizeof(error), "unexpected boot message of %zd bytes", received);
    } else if (message.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) {
        snprintf(error, sizeof(error), "truncated message, flags %#x", (unsigned)message.msg_flags);
    } else if (hello_length != (ssize_t)sizeof(hello_message) || memcmp(hello, hello_message, sizeof(hello_message)) != 0) {
        snprintf(error, sizeof(error), "no hello: %s", hello_length > 0 ? hello : "the zygote closed the socket");
    } else if (sender != (pid_t)pid) {
        snprintf(error, sizeof(error), "SCM_CREDENTIALS reported PID %d for child %ld", (int)sender, pid);
    } else if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        snprintf(error, sizeof(error), "the zygote ended with wait status %#x", (unsigned)status);
    }

report:
    if (error[0]) {
        printf("{\"probe\": \"chromium_%s\", \"status\": \"failed\", \"error\": \"%s\"}\n", name, error);
        return 1;
    }
    printf("{\"probe\": \"chromium_%s\", \"status\": \"passed\"}\n", name);
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--zygote") == 0) return zygote_main(0);
    if (argc == 2 && strcmp(argv[1], "--zygote-sandboxed") == 0) return zygote_main(1);
    // Chromium already runs threads when it checks the sandbox.
    pthread_t thread;
    if (pthread_create(&thread, NULL, idle, NULL) != 0) return 1;
    setvbuf(stdout, NULL, _IONBF, 0);
    int failed = 0;
    long links = task_links();
    if (links == 4) {
        puts("{\"probe\": \"chromium_thread_count\", \"status\": \"passed\"}");
    } else {
        printf("{\"probe\": \"chromium_thread_count\", \"status\": \"failed\", \"error\": \"/proc/self/task has %ld links with two threads\"}\n", links);
        failed = 1;
    }
    // Chromium only creates the namespace types whose /proc/self/ns entry exists.
    // Reference: NamespaceUtils::KernelSupportsUnprivilegedNamespace() in Chromium.
    static const char *const namespace_files[] = {"/proc/self/ns/user", "/proc/self/ns/pid", "/proc/self/ns/net"};
    for (size_t index = 0; index < sizeof(namespace_files) / sizeof(namespace_files[0]); index++) {
        if (access(namespace_files[index], F_OK) != 0) {
            printf("{\"probe\": \"chromium_namespace_files\", \"status\": \"failed\", \"error\": \"%s: %s\"}\n",
                   namespace_files[index], strerror(errno));
            failed = 1;
        }
    }
    if (!failed) puts("{\"probe\": \"chromium_namespace_files\", \"status\": \"passed\"}");
    failed |= check("user_namespace", CLONE_NEWUSER);
    failed |= check("namespace_sandbox", CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET);
    failed |= check_clone_helpers();
    failed |= check_socket_passing();
    failed |= check_address_tracker();
    failed |= check_process_title(argv);
    failed |= check_zygote("zygote_plain", 0);
    failed |= check_zygote("zygote_sandboxed", CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET);
    if (!failed) puts("OPENSHELL_NATIVE_CHROMIUM_USERNS_OK");
    return failed;
}
