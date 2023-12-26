# Rina - A Nina implementation in Rust

Since Nina implementation in NodeJs stopped working, I've decided to start a new implementation using a more performant language. **Rina** is not supposed to be used in big scale, it is meant to suffice personal use cases/servers.

## Quick Start

I'm going to configure a docker-compose to easily build/run the application in the future, but for now, you can do it manually following the instructions bellow. 

### Build Rina

This repository contains a Dockerfile configuration to improve the build experience. All you need to have is [docker](https://www.docker.com/) installed in your machine. Then run the following command to build the docker image:

```console
docker build . -t rina-image
```

### Running Rina

After building the image, all you gotta do is start the container with the following:

```console
docker run --name rina -d rina-image
```

### Todo

[] add `!help` command
[] support playlist
[] inform track end with track's title
[] investigate possible memory leak 