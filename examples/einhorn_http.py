#!/usr/bin/env python3
"""
This small example program demonstrates one way to integerate with Einhorn using
Python (3).

It serves up the current working directory over HTTP on either the
Einhorn-supplied socket or localhost:8080.
"""

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
            raise EnvironmentError("Couldn't find EINHORN_FD_0 env variable... is this running under einhorn?")

        self.socket = socket.socket(fileno=fd)
        # alternative?
        #self.socket = socket.fromfd(socket.AF_INET, socket.SOCK_STREAM, fd)

        try:
            self.server_activate()
        except:
            self.server_close()
            raise

if __name__ == "__main__":
    Handler = http.server.SimpleHTTPRequestHandler
    try:
        httpd = EinhornTCPServer(None, Handler)
    except EnvironmentError as ee:
        print(ee)
        print("Falling back on vanilla http server on 8080")
        httpd = socketserver.TCPServer(("localhost", 8080), Handler)

    print("Serving!")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("Caught KeyboardInterrupt, shutting down")
        httpd.server_close()
