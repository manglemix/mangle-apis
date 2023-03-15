## docker_build_env
A container made to imitate an AWS EC2 instance. I do it this way instead of Cross because the system is really
simple and I'd rather develop it from the ground up instead of learn how to use yet another tool. This way, I could easily allow
the devcontainer (which uses the same image) to use the same build caches and `CARGO_HOME`
