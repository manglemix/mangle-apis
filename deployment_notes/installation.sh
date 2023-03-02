# Install docker
sudo yum install docker
sudo systemctl start docker
sudo systemctl enable docker

# Install docker-compose
sudo sudo curl -L https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m) -o /bin/docker-compose
sudo sudo chmod +x /bin/docker-compose

# Install snap
curl -o snap-confine-2.36.3-0.amzn2.x86_64.rpm -L https://github.com/albuild/snap/releases/download/v0.1.0/snap-confine-2.36.3-0.amzn2.x86_64.rpm
curl -o snapd-2.36.3-0.amzn2.x86_64.rpm -L https://github.com/albuild/snap/releases/download/v0.1.0/snapd-2.36.3-0.amzn2.x86_64.rpm
sudo yum -y install snap-confine-0.1.0-0.amzn2.x86_64.rpm snapd-0.1.0-0.amzn2.x86_64.rpm
sudo systemctl enable --now snapd.socket
sudo snap install core
sudo snap refresh core

# Install certbot
sudo snap install --classic certbot
sudo ln -s /snap/bin/certbot /usr/bin/certbot