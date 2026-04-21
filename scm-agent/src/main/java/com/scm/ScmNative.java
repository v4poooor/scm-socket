package com.scm;

import io.netty.channel.ChannelFutureListener;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.EventLoopGroup;
import io.netty.channel.SimpleChannelInboundHandler;
import io.netty.channel.epoll.EpollDomainSocketChannel;
import io.netty.channel.epoll.EpollEventLoopGroup;
import io.netty.handler.codec.LengthFieldBasedFrameDecoder;
import io.netty.handler.codec.LengthFieldPrepender;
import io.netty.handler.codec.string.StringDecoder;
import io.netty.handler.codec.string.StringEncoder;
import io.netty.util.CharsetUtil;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.InputStream;
import java.lang.instrument.Instrumentation;
import java.nio.file.Files;

public class ScmNative {

    public static native int receiveFdFromBootstrap();

    static {
        loadLibrary();
    }

    private static void loadLibrary() {
        String libName = "libscm_jni.so";
        try {
            File tempLib = new File(System.getProperty("java.io.tmpdir"), libName);
            try (InputStream in = ScmNative.class.getResourceAsStream("/" + libName)) {
                if (in == null) {
                    throw new FileNotFoundException("JAR 内未找到 " + libName);
                }
                Files.copy(in, tempLib.toPath(), java.nio.file.StandardCopyOption.REPLACE_EXISTING);
            }
            System.load(tempLib.getAbsolutePath());
            System.out.println("[Agent] 成功加载动态库: " + tempLib.getAbsolutePath());
        } catch (Exception e) {
            System.err.println("[Agent] 加载失败: " + e.getMessage());
            e.printStackTrace();
        }
    }

    private static EventLoopGroup group = new EpollEventLoopGroup(1);

    public static void premain(String agentArgs, Instrumentation inst) {
        agentmain(agentArgs, inst);
    }

    public static void agentmain(String agentArgs, Instrumentation inst) {
        try {
            System.out.println("[Agent] 已注入，启动 Netty 引导序列...");
            startNettyChannel();
        } catch (Exception e) {
            e.printStackTrace();
        }
    }

    private static void startNettyChannel() throws Exception {
        // 将 int fd 封装为 Netty 的 FileDescriptor
        int fd = receiveFdFromBootstrap();
        if (fd == -1) {
            System.err.println("[Agent] 接收 socket fd 失败: " + fd);
            return ;
        }

        // 注意：由于是连接已建立的流，直接创建 EpollDomainSocketChannel 并注册
        EpollDomainSocketChannel channel = new EpollDomainSocketChannel(fd);

        channel.pipeline().addLast(
                // 4 字节长度头解码 (最大 5MB)
                new LengthFieldBasedFrameDecoder(5 * 1024 * 1024, 0, 4, 0, 4),
                // 自动在发送时添加 4 字节长度头
                new LengthFieldPrepender(4),
                new StringDecoder(CharsetUtil.UTF_8),
                new StringEncoder(CharsetUtil.UTF_8),
                new SimpleChannelInboundHandler<String>() {
                    @Override
                    protected void channelRead0(ChannelHandlerContext ctx, String msg) {
                        System.out.println("[Agent] 收到请求: " + msg);
                        String name = Thread.currentThread().getName();
                        String response = name + ": agent health check";
                        ctx.writeAndFlush(response);
                    }

                    @Override
                    public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
                        System.err.println("[Agent] 通讯异常: " + cause.getMessage());
                        ctx.close();
                    }
                }
        );
        // 绑定 EventLoop 开始监听
        group.register(channel).addListener((ChannelFutureListener) future -> {
            if (future.isSuccess()) {
                System.out.println("[Agent] Netty 成功接管 FD，监听中...");
            } else {
                System.err.println("[Agent] Netty 注册失败");
            }
        });
    }
}
