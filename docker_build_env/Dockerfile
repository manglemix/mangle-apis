FROM amazonlinux

RUN yum install gcc pkgconfig openssl-devel -y
RUN curl https://sh.rustup.rs -sSf | bash -s -- --default-toolchain nightly -y

RUN echo 'source $HOME/.cargo/env' >> $HOME/.bashrc

RUN yum install xz tar git -y
RUN curl -OL https://github.com/upx/upx/releases/download/v4.0.2/upx-4.0.2-amd64_linux.tar.xz && \
    tar -xf /upx-4.0.2-amd64_linux.tar.xz && rm upx-4.0.2-amd64_linux.tar.xz
ENV PATH="${PATH}:/upx-4.0.2-amd64_linux:/"

COPY bashrc_append /
# Append bashrc
RUN ["/bin/bash", "-c", "cat /bashrc_append >> /root/.bashrc && rm /bashrc_append"]