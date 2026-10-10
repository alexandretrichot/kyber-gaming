# Never run as a container: the VM agent extracts these layers onto the host.
FROM scratch
ARG KYBER_REV
LABEL org.kyber-gaming.kyber.rev=${KYBER_REV}
COPY kyber/ /opt/kg/kyber/
