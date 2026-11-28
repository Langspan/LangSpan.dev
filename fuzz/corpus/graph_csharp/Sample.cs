using System;

namespace Demo
{
    public interface IListener
    {
        void Listen();
    }

    public class Server
    {
        public int Port { get; set; }

        public void Serve() => Bind(Port);

        static void Bind(int port)
        {
            Console.WriteLine(port);
        }
    }
}
