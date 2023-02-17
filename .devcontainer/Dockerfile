FROM amazonlinux

RUN yum install gcc pkgconfig openssl-devel -y
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y

RUN echo 'source $HOME/.cargo/env' >> $HOME/.bashr

RUN yum install xz tar -y
RUN curl -OL https://github.com/upx/upx/releases/download/v4.0.2/upx-4.0.2-amd64_linux.tar.xz
RUN tar -xf /upx-4.0.2-amd64_linux.tar.xz && rm upx-4.0.2-amd64_linux.tar.xz
RUN export PATH=/upx-4.0.2-amd64_linux:$PATH