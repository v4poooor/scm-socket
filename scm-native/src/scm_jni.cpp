#include <jni.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <vector>
#include <cstdio>
#include <cstring>
#include <cerrno>
#include "com_scm_ScmNative.h"

extern "C" JNIEXPORT jint JNICALL Java_com_scm_ScmNative_receiveFdFromBootstrap(JNIEnv *env, jclass clazz) {
    const char* content = "rasptempsock";
    jstring name = env->NewStringUTF(content);
    const char *sock_name = env->GetStringUTFChars(name, nullptr);
    fprintf(stderr, "[SCM-NATIVE] 尝试连接抽象套接字: \\0%s\n", sock_name);
    // 1. 创建 Unix Domain Socket
    int sock = socket(AF_UNIX, SOCK_STREAM, 0);
    if (sock < 0) {
        fprintf(stderr, "[SCM-NATIVE] 创建 Socket 失败: %s\n", strerror(errno));
        env->ReleaseStringUTFChars(name, sock_name);
        return -1;
    }
    // 2. 构造抽象套接字地址
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;

    size_t name_len = strlen(sock_name);
    addr.sun_path[0] = '\0'; 
    memcpy(&addr.sun_path[1], sock_name, name_len);

    socklen_t addr_len = offsetof(struct sockaddr_un, sun_path) + 1 + name_len;
    fprintf(stderr, "[SCM] 正在连接到 @%s...\n", sock_name);
    // sun_path[0] = '\0', 后面留空，由内核自动分配抽象地址
    if (connect(sock, (struct sockaddr *)&addr, addr_len) < 0) {
        fprintf(stderr, "[SCM-NATIVE] 客户端 bind 失败: %s\n", strerror(errno));
        close(sock);
        env->ReleaseStringUTFChars(name, sock_name);
        return -1;
    }
    // 3. 握手：发送探测包
    char handshake = '!';
    ssize_t sent = send(sock, &handshake, 1, MSG_NOSIGNAL);
    if (sent < 0) {
        if (errno == EPIPE) {
            fprintf(stderr, "[SCM-NATIVE] 错误: Broken pipe。服务端已关闭连接，请检查 Rust Server 日志。\n");
        } else {
            fprintf(stderr, "[SCM-NATIVE] 发送失败: %s\n", strerror(errno));
        }
        close(sock);
        env->ReleaseStringUTFChars(name, sock_name);
        return -1;
    }
    // 4. 准备接收 SCM_RIGHTS 消息
    struct msghdr msg = {0};
    char iov_buf[1];
    struct iovec iov = {.iov_base = iov_buf, .iov_len = 1};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    char cmsg_buf[CMSG_SPACE(sizeof(int))];
    msg.msg_control = cmsg_buf;
    msg.msg_controllen = sizeof(cmsg_buf);
    // 5. 阻塞接收
    ssize_t n = recvmsg(sock, &msg, 0);
    if (n < 0) {
        fprintf(stderr, "[SCM-NATIVE] 接收消息失败 (recvmsg): %s\n", strerror(errno));
        close(sock);
        env->ReleaseStringUTFChars(name, sock_name);
        return -1;
    }
    // 从辅助数据中提取 FD
    int received_fd = -1;
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    while (cmsg != nullptr) {
        if (cmsg->cmsg_level == SOL_SOCKET && cmsg->cmsg_type == SCM_RIGHTS) {
            received_fd = *((int *)CMSG_DATA(cmsg));
            break;
        }
        cmsg = CMSG_NXTHDR(&msg, cmsg);
    }
    close(sock); 
    env->ReleaseStringUTFChars(name, sock_name);
    if (received_fd == -1) {
        fprintf(stderr, "[SCM-NATIVE] 错误: 未能在控制消息中找到 SCM_RIGHTS 数据。\n");
    }
    return received_fd;
}
