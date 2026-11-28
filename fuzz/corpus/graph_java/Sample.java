package demo;

import java.util.List;

public class Server {
    private int port;

    public void serve(List<String> args) {
        bind(port);
    }

    static int bind(int port) {
        return port;
    }
}

interface Listener {
    void listen();
}
