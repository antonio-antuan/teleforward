ARG rust_version=1.74.0
# want to get specific tdlib version. tdlib does not have tags for each version
# this default refers to 1.8.21
ARG tdlib_ref=3870c29b158b75ca5e48e0eebd6b5c3a7994a000

FROM rust:${rust_version}-slim-bookworm

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive TZ=Etc/UTC apt-get install -y -q \
    make git zlib1g-dev libssl-dev gperf php-cli cmake g++

# clone specific tdlib version
RUN git clone https://github.com/tdlib/td.git /td && \
    cd /td && \
    git checkout ${tdlib_ref} && \
    mkdir build && \
    cd build && \
    cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX:PATH=/usr/local/ .. && \
    cmake --build . --target install && \
    cd .. && \
    cd .. && \
    ls -l /usr/local && \
    rm -rf /td

COPY . /usr/src/myapp
WORKDIR /usr/src/myapp
RUN cargo build && \
    cp target/release/teleforward /opt/teleforward && \
    rm -rf /usr/src/myapp


ENTRYPOINT ["/opt/teleforward"]
