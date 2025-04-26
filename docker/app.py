from flask import Flask, jsonify
import os

app = Flask(__name__)
hostname = os.environ.get('HOSTNAME', 'unknown')


@app.route('/')
def base():
    return jsonify({
        'message': 'Hello from Python server!',
        'server_id': hostname
    })

@app.route('/hello')
def hello():
    return jsonify({
        'message': 'Hello world from Flask!',
        'server_id': hostname
    })

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=5000) 