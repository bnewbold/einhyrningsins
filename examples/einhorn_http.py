#!/usr/bin/python3

import os
import sys
import socket
import socketserver
import http.server

class EinhornTCPServer(socketserver.TCPServer):

    def __init__(self, server_address, RequestHandlerClass):
        socketserver.BaseServer.__init__(self, server_address, RequestHandlerClass)

        # Try to sniff first socket
        try:
            fd = int(os.environ['EINHORN_FD_0'])
            print("Will try to listen with fd=%d" % fd)
        except KeyError:
            print("Couldn't find EINHORN_FD_0 env variable... is this running under einhorn?")
            sys.exit(1)

        #self.socket = socket.fromfd(socket.AF_INET, socket.SOCK_STREAM, fd)
        self.socket = socket.socket(fileno=fd)

        try:
            self.server_activate()
        except:
            self.server_close()
            raise

if __name__ == "__main__":
    Handler = http.server.SimpleHTTPRequestHandler
    try:
        httpd = EinhornTCPServer(None, Handler)
    except:
        print("Falling back on vanilla http server on 8080")
        httpd = socketserver.TCPServer(("localhost", 8080), Handler)

    print("Serving!")
    httpd.serve_forever()
