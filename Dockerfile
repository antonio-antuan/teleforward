FROM rust:1.74.0-slim-bookworm

RUN apt-get update
RUN DEBIAN_FRONTEND=noninteractive TZ=Etc/UTC apt-get install -y -q make git zlib1g-dev libssl-dev gperf php-cli cmake g++
RUN git clone https://github.com/tdlib/td.git
WORKDIR /td
RUN mkdir build && \
    cd build && \
    cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX:PATH=/usr/local .. && \
    cmake --build . --target install && \
    cd .. && \
    cd .. && \
    ls -l /usr/local

COPY . /usr/src/myapp
WORKDIR /usr/src/myapp
RUN cargo build --release

ENTRYPOINT ["/opt/teleforward"]
